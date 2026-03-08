use crate::{
    ConfigSource, ConversionRecommendation, ConvertInventory, MCPServerProfile, PermissionLevel,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn discover_from_sources(sources: &[ConfigSource]) -> Result<ConvertInventory> {
    let mut servers = Vec::new();

    for source in sources {
        if !source.path.exists() {
            continue;
        }

        let raw = std::fs::read_to_string(&source.path)
            .with_context(|| format!("Failed to read {}", source.path.display()))?;
        let root = parse_source_root(&raw, &source.path)?;

        for (name, entry) in extract_server_entries(&root) {
            let Some(obj) = entry.as_object() else {
                continue;
            };

            let permission_hints = collect_permission_hints(obj);
            let command = obj
                .get("command")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let args = obj
                .get("args")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let url = obj
                .get("url")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    obj.get("endpoint")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                });
            let env_keys = obj
                .get("env")
                .and_then(Value::as_object)
                .map(|env| {
                    let mut keys = env.keys().cloned().collect::<Vec<_>>();
                    keys.sort();
                    keys
                })
                .unwrap_or_default();
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
                .or_else(|| {
                    obj.get("purpose")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToString::to_string)
                });
            let declared_tool_count = declared_tool_count(obj);
            let inferred_permission = infer_permission(
                &name,
                description.as_deref(),
                command.as_deref(),
                &args,
                &permission_hints,
            );
            let purpose = infer_purpose(
                &name,
                description.as_deref(),
                command.as_deref(),
                url.as_deref(),
                &args,
            );
            let (recommendation, recommendation_reason) = recommend_conversion(
                inferred_permission.clone(),
                url.as_deref(),
                &env_keys,
                declared_tool_count,
            );

            servers.push(MCPServerProfile {
                id: format!("{}:{}", source.label, name),
                name,
                source_label: source.label.clone(),
                source_path: source.path.clone(),
                purpose,
                command,
                args,
                url,
                env_keys,
                declared_tool_count,
                permission_hints,
                inferred_permission,
                recommendation,
                recommendation_reason,
            });
        }
    }

    servers.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(ConvertInventory {
        generated_at: Utc::now(),
        searched_paths: sources.iter().map(|s| s.path.clone()).collect(),
        servers,
    })
}

fn parse_source_root(raw: &str, path: &Path) -> Result<Value> {
    if let Ok(root) = serde_json::from_str::<Value>(raw) {
        return Ok(root);
    }

    if let Ok(toml_root) = toml::from_str::<toml::Value>(raw) {
        return serde_json::to_value(toml_root)
            .with_context(|| format!("Failed to convert TOML in {}", path.display()));
    }

    bail!(
        "Failed to parse {} as JSON or TOML MCP config.",
        path.display()
    )
}

pub(crate) fn default_sources(
    home: &Path,
    cwd: &Path,
    additional_paths: &[PathBuf],
) -> Vec<ConfigSource> {
    let mut sources = vec![
        ConfigSource {
            label: "claude-global-json".to_string(),
            path: home.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "claude-global-settings".to_string(),
            path: home.join(".claude").join("settings.json"),
        },
        ConfigSource {
            label: "claude-project-json".to_string(),
            path: cwd.join(".claude").join("mcp.json"),
        },
        ConfigSource {
            label: "claude-project-settings".to_string(),
            path: cwd.join(".claude").join("settings.json"),
        },
        ConfigSource {
            label: "codex-global-json".to_string(),
            path: home.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-global-toml".to_string(),
            path: home.join(".codex").join("config.toml"),
        },
        ConfigSource {
            label: "codex-project-json".to_string(),
            path: cwd.join(".codex").join("mcp.json"),
        },
        ConfigSource {
            label: "codex-project-toml".to_string(),
            path: cwd.join(".codex").join("config.toml"),
        },
        ConfigSource {
            label: "shared-global".to_string(),
            path: home.join(".config").join("mcp").join("servers.json"),
        },
        ConfigSource {
            label: "amp-settings".to_string(),
            path: home.join(".config").join("amp").join("settings.json"),
        },
    ];

    for (idx, path) in additional_paths.iter().enumerate() {
        sources.push(ConfigSource {
            label: format!("custom-{}", idx + 1),
            path: path.clone(),
        });
    }

    dedupe_sources(sources)
}

fn dedupe_sources(sources: Vec<ConfigSource>) -> Vec<ConfigSource> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for source in sources {
        let key = source.path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            out.push(source);
        }
    }
    out
}

fn extract_server_entries(root: &Value) -> Vec<(String, Value)> {
    if let Some(obj) = root.get("mcpServers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("mcp_servers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("servers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(obj) = root.get("amp.mcpServers").and_then(Value::as_object) {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(amp_obj) = root.get("amp").and_then(Value::as_object)
        && let Some(obj) = amp_obj.get("mcpServers").and_then(Value::as_object)
    {
        return obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }

    if let Some(obj) = root.as_object() {
        return obj
            .iter()
            .filter(|(_, value)| likely_server_object(value))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
    }

    Vec::new()
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

fn declared_tool_count(obj: &serde_json::Map<String, Value>) -> usize {
    if let Some(arr) = obj.get("tools").and_then(Value::as_array) {
        return arr.len();
    }
    if let Some(num) = obj.get("tool_count").and_then(Value::as_u64) {
        return num as usize;
    }
    if let Some(cap) = obj.get("capabilities").and_then(Value::as_object) {
        if let Some(arr) = cap.get("tools").and_then(Value::as_array) {
            return arr.len();
        }
        if let Some(num) = cap.get("tool_count").and_then(Value::as_u64) {
            return num as usize;
        }
    }
    0
}

fn collect_permission_hints(obj: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut hints = BTreeSet::new();

    for key in ["permissions", "scopes"] {
        match obj.get(key) {
            Some(Value::String(s)) => {
                hints.insert(s.trim().to_lowercase());
            }
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(s) = item.as_str() {
                        hints.insert(s.trim().to_lowercase());
                    }
                }
            }
            _ => {}
        }
    }

    for key in ["readOnly", "read_only", "readonly"] {
        if obj.get(key).and_then(Value::as_bool) == Some(true) {
            hints.insert("read-only".to_string());
        }
    }

    if let Some(cap) = obj.get("capabilities").and_then(Value::as_object) {
        for (k, v) in cap {
            if v.as_bool() == Some(true) {
                hints.insert(k.to_lowercase());
            }
        }
    }

    hints.into_iter().collect()
}

fn infer_purpose(
    name: &str,
    description: Option<&str>,
    command: Option<&str>,
    url: Option<&str>,
    args: &[String],
) -> String {
    if let Some(desc) = description {
        return desc.to_string();
    }

    let mut haystack = vec![name.to_lowercase()];
    if let Some(cmd) = command {
        haystack.push(cmd.to_lowercase());
    }
    if let Some(endpoint) = url {
        haystack.push(endpoint.to_lowercase());
    }
    haystack.extend(args.iter().map(|arg| arg.to_lowercase()));
    let corpus = haystack.join(" ");

    if contains_any(&corpus, &["playwright", "browser", "puppeteer", "selenium"]) {
        return "Browser automation and interactive web workflows".to_string();
    }
    if contains_any(&corpus, &["xcode", "simulator", "ios", "xcodebuildmcp"]) {
        return "Xcode build, simulator, and iOS debug workflows".to_string();
    }
    if contains_any(&corpus, &["chrome-devtools", "devtools", "chrome"]) {
        return "Browser inspection and debugging workflows".to_string();
    }
    if contains_any(
        &corpus,
        &["memory", "knowledge graph", "read_graph", "search_nodes"],
    ) {
        return "Memory and knowledge graph workflows".to_string();
    }
    if contains_any(
        &corpus,
        &[
            "jira",
            "linear",
            "github",
            "gitlab",
            "issue",
            "pull request",
            "merge request",
        ],
    ) {
        return "Project and issue management workflows".to_string();
    }
    if contains_any(
        &corpus,
        &["k8s", "kubectl", "helm", "terraform", "aws", "gcloud"],
    ) {
        return "Infrastructure and platform operations".to_string();
    }
    if contains_any(&corpus, &["sql", "postgres", "mysql", "database", "db"]) {
        return "Database querying and administration".to_string();
    }
    if contains_any(&corpus, &["file", "filesystem", "fs", "local", "shell"]) {
        return "Local automation and filesystem tasks".to_string();
    }

    "General-purpose MCP integration".to_string()
}

fn infer_permission(
    name: &str,
    description: Option<&str>,
    command: Option<&str>,
    args: &[String],
    permission_hints: &[String],
) -> PermissionLevel {
    let mut parts = vec![name.to_lowercase()];
    if let Some(desc) = description {
        parts.push(desc.to_lowercase());
    }
    if let Some(cmd) = command {
        parts.push(cmd.to_lowercase());
    }
    parts.extend(args.iter().map(|arg| arg.to_lowercase()));
    parts.extend(permission_hints.iter().map(|hint| hint.to_lowercase()));
    let corpus = parts.join(" ");

    let destructive = [
        "delete",
        "destroy",
        "drop",
        "rm -rf",
        "truncate",
        "uninstall",
        "terminate",
        "shutdown",
    ];
    let write = [
        "write",
        "create",
        "update",
        "insert",
        "upsert",
        "apply",
        "deploy",
        "commit",
        "push",
        "exec",
        "execute",
        "mutation",
        "admin",
        "xcodebuildmcp",
        "xcode",
        "simulator",
        "debug",
        "chrome-devtools",
        "devtools",
    ];
    let read = [
        "read", "list", "get", "search", "query", "fetch", "inspect", "browse",
    ];

    if contains_any(&corpus, &destructive) {
        return PermissionLevel::Destructive;
    }
    if contains_any(&corpus, &write) {
        return PermissionLevel::Write;
    }

    let read_only_hint = permission_hints
        .iter()
        .any(|hint| hint.contains("read") && !hint.contains("write"));
    if read_only_hint || contains_any(&corpus, &read) {
        return PermissionLevel::ReadOnly;
    }

    PermissionLevel::Unknown
}

fn recommend_conversion(
    permission: PermissionLevel,
    url: Option<&str>,
    env_keys: &[String],
    declared_tool_count: usize,
) -> (ConversionRecommendation, String) {
    if url.is_some() {
        return (
            ConversionRecommendation::KeepMcp,
            "Remote URL-based servers are typically dynamic and better kept as MCP integrations."
                .to_string(),
        );
    }

    if permission == PermissionLevel::Destructive {
        return (
            ConversionRecommendation::KeepMcp,
            "Destructive actions detected; keep MCP for explicit execution controls.".to_string(),
        );
    }

    if permission == PermissionLevel::Write {
        return (
            ConversionRecommendation::Hybrid,
            "Write-oriented capabilities are safer as MCP with skills for orchestration."
                .to_string(),
        );
    }

    if permission == PermissionLevel::ReadOnly {
        if env_keys.is_empty() {
            return (
                ConversionRecommendation::ReplaceCandidate,
                "Read-only and no credential requirements; good candidate for skill replacement."
                    .to_string(),
            );
        }
        return (
            ConversionRecommendation::Hybrid,
            "Read-only but credential-backed; prefer hybrid conversion with MCP fallback."
                .to_string(),
        );
    }

    if declared_tool_count > 10 {
        return (
            ConversionRecommendation::Hybrid,
            "Large tool surface detected; start with hybrid conversion and verify incrementally."
                .to_string(),
        );
    }

    (
        ConversionRecommendation::Hybrid,
        "Insufficient metadata for safe replacement; defaulting to hybrid conversion.".to_string(),
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}
