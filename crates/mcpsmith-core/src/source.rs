use crate::{
    ConfigSource, ConversionRecommendation, ConvertInventory, MCPServerProfile, PermissionLevel,
    SourceEvidenceLevel, SourceGrounding, SourceKind,
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
            let source_grounding = resolve_source_grounding(
                &source.path,
                obj,
                command.as_deref(),
                &args,
                url.as_deref(),
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
                source_grounding,
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

fn resolve_source_grounding(
    source_path: &Path,
    obj: &serde_json::Map<String, Value>,
    command: Option<&str>,
    args: &[String],
    url: Option<&str>,
) -> SourceGrounding {
    let mut grounding = SourceGrounding {
        homepage: explicit_homepage(obj),
        repository_url: explicit_repository_url(obj),
        ..SourceGrounding::default()
    };

    if let Some(command) = command {
        if let Some(entrypoint) = resolve_local_command_path(source_path, command) {
            grounding.kind = SourceKind::LocalPath;
            grounding.entrypoint = Some(entrypoint.clone());
            inspect_local_source(&entrypoint, &mut grounding);
            return finalize_source_grounding(grounding);
        }

        if let Some((package_name, package_version)) = resolve_npm_package_spec(command, args) {
            grounding.kind = SourceKind::NpmPackage;
            grounding.package_name = Some(package_name.clone());
            grounding.package_version = package_version;
            inspect_local_node_package(source_path, &package_name, &mut grounding);
            return finalize_source_grounding(grounding);
        }

        if let Some((package_name, package_version)) = resolve_pypi_package_spec(command, args) {
            grounding.kind = SourceKind::PypiPackage;
            grounding.package_name = Some(package_name);
            grounding.package_version = package_version;
            inspect_local_pyproject(source_path, &mut grounding);
            return finalize_source_grounding(grounding);
        }
    }

    grounding.kind = if grounding.repository_url.is_some() {
        SourceKind::RepositoryUrl
    } else if url.is_some() {
        SourceKind::RemoteUrl
    } else {
        SourceKind::Unknown
    };

    finalize_source_grounding(grounding)
}

fn explicit_homepage(obj: &serde_json::Map<String, Value>) -> Option<String> {
    ["homepage", "website"]
        .iter()
        .find_map(|key| json_string(obj.get(*key)))
}

fn explicit_repository_url(obj: &serde_json::Map<String, Value>) -> Option<String> {
    [
        "repository",
        "repo",
        "repositoryUrl",
        "repository_url",
        "source",
        "sourceUrl",
    ]
    .iter()
    .find_map(|key| repository_value_to_url(obj.get(*key)?))
}

fn json_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn repository_value_to_url(value: &Value) -> Option<String> {
    json_string(Some(value)).or_else(|| {
        value.as_object().and_then(|obj| {
            ["url", "href", "web"]
                .iter()
                .find_map(|key| json_string(obj.get(*key)))
        })
    })
}

fn resolve_local_command_path(source_path: &Path, command: &str) -> Option<PathBuf> {
    let looks_like_path =
        command.starts_with('.') || command.contains('/') || command.contains('\\');
    if !looks_like_path && !Path::new(command).is_absolute() {
        return None;
    }

    let path = Path::new(command);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        source_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    };

    Some(std::fs::canonicalize(&resolved).ok().unwrap_or(resolved))
}

fn inspect_local_source(entrypoint: &Path, grounding: &mut SourceGrounding) {
    if entrypoint.exists() {
        grounding.inspected = true;
        grounding.inspected_paths.push(entrypoint.to_path_buf());
    }

    let Some(start_dir) = entrypoint.parent() else {
        return;
    };

    if let Some(package_json) = find_upwards(start_dir, "package.json", 6) {
        merge_package_json_metadata(grounding, &package_json);
    }
    if let Some(pyproject) = find_upwards(start_dir, "pyproject.toml", 6) {
        merge_pyproject_metadata(grounding, &pyproject);
    }
}

fn inspect_local_node_package(
    source_path: &Path,
    package_name: &str,
    grounding: &mut SourceGrounding,
) {
    let Some(start_dir) = source_path.parent() else {
        return;
    };

    for dir in start_dir.ancestors().take(6) {
        let candidate = dir
            .join("node_modules")
            .join(package_name)
            .join("package.json");
        if candidate.exists() {
            merge_package_json_metadata(grounding, &candidate);
            break;
        }
    }
}

fn inspect_local_pyproject(source_path: &Path, grounding: &mut SourceGrounding) {
    let Some(start_dir) = source_path.parent() else {
        return;
    };

    if let Some(pyproject) = find_upwards(start_dir, "pyproject.toml", 6) {
        merge_pyproject_metadata(grounding, &pyproject);
    }
}

fn find_upwards(start_dir: &Path, file_name: &str, max_levels: usize) -> Option<PathBuf> {
    start_dir
        .ancestors()
        .take(max_levels)
        .map(|dir| dir.join(file_name))
        .find(|candidate| candidate.exists())
}

fn merge_package_json_metadata(grounding: &mut SourceGrounding, path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(root) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let Some(obj) = root.as_object() else {
        return;
    };

    grounding.inspected = true;
    grounding.inspected_paths.push(path.to_path_buf());
    if grounding.package_name.is_none() {
        grounding.package_name = json_string(obj.get("name"));
    }
    if grounding.package_version.is_none() {
        grounding.package_version = json_string(obj.get("version"));
    }
    if grounding.homepage.is_none() {
        grounding.homepage = json_string(obj.get("homepage"));
    }
    if grounding.repository_url.is_none() {
        grounding.repository_url = obj
            .get("repository")
            .and_then(repository_value_to_url)
            .or_else(|| json_string(obj.get("repositoryUrl")));
    }
}

fn merge_pyproject_metadata(grounding: &mut SourceGrounding, path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(root) = toml::from_str::<toml::Value>(&raw) else {
        return;
    };

    grounding.inspected = true;
    grounding.inspected_paths.push(path.to_path_buf());

    if let Some(project) = root.get("project").and_then(toml::Value::as_table) {
        if grounding.package_name.is_none() {
            grounding.package_name = toml_string(project.get("name"));
        }
        if grounding.package_version.is_none() {
            grounding.package_version = toml_string(project.get("version"));
        }
        if let Some(urls) = project.get("urls").and_then(toml::Value::as_table) {
            if grounding.homepage.is_none() {
                grounding.homepage =
                    toml_string(urls.get("Homepage")).or_else(|| toml_string(urls.get("homepage")));
            }
            if grounding.repository_url.is_none() {
                grounding.repository_url = toml_string(urls.get("Repository"))
                    .or_else(|| toml_string(urls.get("repository")));
            }
        }
    }

    if let Some(poetry) = root
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("poetry"))
        .and_then(toml::Value::as_table)
    {
        if grounding.package_name.is_none() {
            grounding.package_name = toml_string(poetry.get("name"));
        }
        if grounding.package_version.is_none() {
            grounding.package_version = toml_string(poetry.get("version"));
        }
        if grounding.homepage.is_none() {
            grounding.homepage = toml_string(poetry.get("homepage"));
        }
        if grounding.repository_url.is_none() {
            grounding.repository_url = toml_string(poetry.get("repository"));
        }
    }
}

fn toml_string(value: Option<&toml::Value>) -> Option<String> {
    value
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn resolve_npm_package_spec(command: &str, args: &[String]) -> Option<(String, Option<String>)> {
    let command = command_basename(command);
    if command != "npx" && command != "npm" {
        return None;
    }

    let mut index = 0usize;
    if args.first().map(String::as_str) == Some("exec") {
        index = 1;
    }

    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "-y" | "--yes" | "--quiet" => {
                index += 1;
            }
            "-p" | "--package" => {
                return args
                    .get(index + 1)
                    .and_then(|value| parse_npm_package_spec(value));
            }
            value if value.starts_with('-') => {
                index += 1;
            }
            value => return parse_npm_package_spec(value),
        }
    }

    None
}

fn parse_npm_package_spec(spec: &str) -> Option<(String, Option<String>)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.starts_with('/') {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("npm:") {
        return parse_npm_package_spec(rest);
    }

    if let Some(stripped) = trimmed.strip_prefix('@') {
        if let Some(split_at) = stripped.rfind('@').map(|index| index + 1) {
            let name = &trimmed[..split_at];
            let version = trimmed[split_at + 1..].trim();
            if !version.is_empty() {
                return Some((name.to_string(), Some(version.to_string())));
            }
        }
        return Some((trimmed.to_string(), None));
    }

    if let Some((name, version)) = trimmed.rsplit_once('@')
        && !name.is_empty()
        && !version.trim().is_empty()
    {
        return Some((name.to_string(), Some(version.trim().to_string())));
    }

    Some((trimmed.to_string(), None))
}

fn resolve_pypi_package_spec(command: &str, args: &[String]) -> Option<(String, Option<String>)> {
    if command_basename(command) != "uvx" {
        return None;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--from") {
        return args
            .get(index + 1)
            .and_then(|value| parse_pypi_package_spec(value));
    }

    args.iter()
        .find(|arg| !arg.starts_with('-'))
        .and_then(|value| parse_pypi_package_spec(value))
}

fn parse_pypi_package_spec(spec: &str) -> Option<(String, Option<String>)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.starts_with('/') {
        return None;
    }

    let (name, version) = if let Some((name, version)) = trimmed.split_once("==") {
        (name, Some(version))
    } else {
        (trimmed, None)
    };

    let name = name.split('[').next().unwrap_or(name).trim();
    if name.is_empty() {
        return None;
    }

    Some((
        name.to_string(),
        version
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    ))
}

fn command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
}

fn finalize_source_grounding(mut grounding: SourceGrounding) -> SourceGrounding {
    grounding.inspected_paths.sort();
    grounding.inspected_paths.dedup();

    grounding.evidence_level = if grounding.inspected {
        SourceEvidenceLevel::SourceInspected
    } else if grounding.kind != SourceKind::Unknown
        || grounding.entrypoint.is_some()
        || grounding.package_name.is_some()
        || grounding.homepage.is_some()
        || grounding.repository_url.is_some()
    {
        SourceEvidenceLevel::ConfigOnly
    } else {
        SourceEvidenceLevel::RuntimeOnly
    };

    grounding
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_resolves_npx_package_source_grounding() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp@1.55.0"],
      "homepage": "https://playwright.dev",
      "repository": "https://github.com/microsoft/playwright-mcp"
    }
  }
}"#,
        )
        .unwrap();

        let inventory = discover_from_sources(&[ConfigSource {
            label: "fixture".to_string(),
            path: config_path,
        }])
        .unwrap();
        let server = &inventory.servers[0];
        assert_eq!(server.source_grounding.kind, SourceKind::NpmPackage);
        assert_eq!(
            server.source_grounding.evidence_level,
            SourceEvidenceLevel::ConfigOnly
        );
        assert_eq!(
            server.source_grounding.package_name.as_deref(),
            Some("@playwright/mcp")
        );
        assert_eq!(
            server.source_grounding.package_version.as_deref(),
            Some("1.55.0")
        );
    }

    #[test]
    fn discover_resolves_uvx_package_source_grounding() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "memory": {
      "command": "uvx",
      "args": ["--from", "acme-memory-mcp==0.4.1", "memory-mcp-server"],
      "repository": "https://github.com/acme/memory-mcp"
    }
  }
}"#,
        )
        .unwrap();

        let inventory = discover_from_sources(&[ConfigSource {
            label: "fixture".to_string(),
            path: config_path,
        }])
        .unwrap();
        let server = &inventory.servers[0];
        assert_eq!(server.source_grounding.kind, SourceKind::PypiPackage);
        assert_eq!(
            server.source_grounding.package_name.as_deref(),
            Some("acme-memory-mcp")
        );
        assert_eq!(
            server.source_grounding.package_version.as_deref(),
            Some("0.4.1")
        );
    }

    #[test]
    fn discover_inspects_local_executable_source_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let tool_root = dir.path().join("local-tool");
        let bin_dir = tool_root.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let executable = bin_dir.join("server.sh");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(
            tool_root.join("package.json"),
            r#"{
  "name": "@acme/local-mcp",
  "version": "1.2.3",
  "homepage": "https://example.com/local-mcp",
  "repository": {
    "type": "git",
    "url": "https://github.com/acme/local-mcp"
  }
}"#,
        )
        .unwrap();

        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            format!(
                r#"{{
  "mcpServers": {{
    "local-tool": {{
      "command": "{}",
      "description": "Local MCP"
    }}
  }}
}}"#,
                executable.display()
            ),
        )
        .unwrap();

        let inventory = discover_from_sources(&[ConfigSource {
            label: "fixture".to_string(),
            path: config_path,
        }])
        .unwrap();
        let server = &inventory.servers[0];
        assert_eq!(server.source_grounding.kind, SourceKind::LocalPath);
        assert_eq!(
            server.source_grounding.evidence_level,
            SourceEvidenceLevel::SourceInspected
        );
        assert!(server.source_grounding.inspected);
        assert_eq!(
            server.source_grounding.package_name.as_deref(),
            Some("@acme/local-mcp")
        );
    }
}
