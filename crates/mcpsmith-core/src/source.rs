use crate::{
    ConfigSource, ConversionRecommendation, ConvertInventory, DerivationEvidence,
    DerivationEvidenceKind, MCPServerProfile, PermissionLevel, SourceEvidenceLevel,
    SourceGrounding, SourceKind,
};
use anyhow::{Context, Result, bail};
use base64::Engine;
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

pub fn discover_inventory(additional_paths: &[PathBuf]) -> Result<ConvertInventory> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sources = default_sources(&home, &cwd, additional_paths);
    discover_from_sources(&sources)
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
            inspect_remote_npm_package(&package_name, &mut grounding);
            inspect_remote_repository_manifest(&mut grounding);
            return finalize_source_grounding(grounding);
        }

        if let Some((package_name, package_version)) = resolve_pypi_package_spec(command, args) {
            grounding.kind = SourceKind::PypiPackage;
            grounding.package_name = Some(package_name);
            grounding.package_version = package_version;
            inspect_local_pyproject(source_path, &mut grounding);
            inspect_remote_pypi_package(&mut grounding);
            inspect_remote_repository_manifest(&mut grounding);
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

    inspect_remote_repository_manifest(&mut grounding);
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
        if let Ok(raw) = std::fs::read_to_string(entrypoint) {
            record_derivation_evidence(
                grounding,
                DerivationEvidenceKind::EntrypointSnippet,
                entrypoint.display().to_string(),
                &raw,
            );
        }
        inspect_local_command_help(entrypoint, grounding);
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
    if let Some(readme) = find_readme(start_dir, 6)
        && let Ok(raw) = std::fs::read_to_string(&readme)
    {
        record_source_inspection(grounding, Some(&readme), None);
        record_derivation_evidence(
            grounding,
            DerivationEvidenceKind::ReadmeSnippet,
            readme.display().to_string(),
            &raw,
        );
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
    merge_package_json_text(grounding, &raw, Some(path), None);
}

fn merge_pyproject_metadata(grounding: &mut SourceGrounding, path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    merge_pyproject_text(grounding, &raw, Some(path), None);
}

fn merge_package_json_text(
    grounding: &mut SourceGrounding,
    raw: &str,
    source_path: Option<&Path>,
    source_url: Option<&str>,
) {
    let Ok(root) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let Some(obj) = root.as_object() else {
        return;
    };

    record_source_inspection(grounding, source_path, source_url);
    record_derivation_evidence(
        grounding,
        DerivationEvidenceKind::ManifestSnippet,
        source_path
            .map(|path| path.display().to_string())
            .or_else(|| source_url.map(ToString::to_string))
            .unwrap_or_else(|| "package.json".to_string()),
        raw,
    );
    merge_package_json_object(grounding, obj);
    if let Some(readme) = obj.get("readme").and_then(Value::as_str) {
        record_derivation_evidence(
            grounding,
            DerivationEvidenceKind::ReadmeSnippet,
            source_path
                .map(|path| path.display().to_string())
                .or_else(|| source_url.map(ToString::to_string))
                .unwrap_or_else(|| "package.json#readme".to_string()),
            readme,
        );
    }
}

fn merge_package_json_object(
    grounding: &mut SourceGrounding,
    obj: &serde_json::Map<String, Value>,
) {
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

fn merge_pyproject_text(
    grounding: &mut SourceGrounding,
    raw: &str,
    source_path: Option<&Path>,
    source_url: Option<&str>,
) {
    let Ok(root) = toml::from_str::<toml::Value>(raw) else {
        return;
    };

    record_source_inspection(grounding, source_path, source_url);
    record_derivation_evidence(
        grounding,
        DerivationEvidenceKind::ManifestSnippet,
        source_path
            .map(|path| path.display().to_string())
            .or_else(|| source_url.map(ToString::to_string))
            .unwrap_or_else(|| "pyproject.toml".to_string()),
        raw,
    );
    merge_pyproject_value(grounding, &root);
}

fn merge_pyproject_value(grounding: &mut SourceGrounding, root: &toml::Value) {
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

fn record_source_inspection(
    grounding: &mut SourceGrounding,
    source_path: Option<&Path>,
    source_url: Option<&str>,
) {
    grounding.inspected = true;
    if let Some(path) = source_path {
        grounding.inspected_paths.push(path.to_path_buf());
    }
    if let Some(url) = source_url {
        grounding.inspected_urls.push(url.to_string());
    }
}

fn record_derivation_evidence(
    grounding: &mut SourceGrounding,
    kind: DerivationEvidenceKind,
    source: String,
    raw: &str,
) {
    let excerpt = excerpt_text(raw, 900);
    if excerpt.is_empty() {
        return;
    }
    grounding.derivation_evidence.push(DerivationEvidence {
        kind,
        source,
        excerpt,
    });
}

fn excerpt_text(raw: &str, max_chars: usize) -> String {
    let normalized = raw
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(24)
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let clipped = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{clipped}...")
    } else {
        clipped
    }
}

fn find_readme(start_dir: &Path, max_levels: usize) -> Option<PathBuf> {
    const CANDIDATES: [&str; 5] = ["README.md", "Readme.md", "readme.md", "README", "readme"];

    start_dir.ancestors().take(max_levels).find_map(|dir| {
        CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
    })
}

fn inspect_local_command_help(entrypoint: &Path, grounding: &mut SourceGrounding) {
    for arg in ["--help", "-h"] {
        let Ok(output) = Command::new(entrypoint).arg(arg).output() else {
            continue;
        };
        let stdout = String::from_utf8(output.stdout).unwrap_or_default();
        let stderr = String::from_utf8(output.stderr).unwrap_or_default();
        let combined = format!("{}\n{}", stdout.trim(), stderr.trim())
            .trim()
            .to_string();
        if combined.is_empty() {
            continue;
        }
        record_derivation_evidence(
            grounding,
            DerivationEvidenceKind::CliHelp,
            format!("{} {}", entrypoint.display(), arg),
            &combined,
        );
        break;
    }
}

fn toml_string(value: Option<&toml::Value>) -> Option<String> {
    value
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn inspect_remote_npm_package(package_name: &str, grounding: &mut SourceGrounding) {
    if !remote_source_fetch_enabled() || grounding.inspected {
        return;
    }

    let base = npm_registry_base_url();
    let encoded_name = url_encode_path_segment(package_name);
    let endpoint = if let Some(version) = grounding.package_version.as_deref() {
        format!(
            "{}/{}/{}",
            base.trim_end_matches('/'),
            encoded_name,
            url_encode_path_segment(version)
        )
    } else {
        format!("{}/{}", base.trim_end_matches('/'), encoded_name)
    };

    let Some(body) = fetch_remote_text(&endpoint, Some("application/json")) else {
        return;
    };
    let Ok(root) = serde_json::from_str::<Value>(&body) else {
        return;
    };

    let manifest = if grounding.package_version.is_some() {
        root
    } else {
        select_npm_manifest(&root, grounding)
    };

    let Some(obj) = manifest.as_object() else {
        return;
    };

    match serde_json::to_string_pretty(&manifest) {
        Ok(raw) => merge_package_json_text(grounding, &raw, None, Some(&endpoint)),
        Err(_) => {
            record_source_inspection(grounding, None, Some(&endpoint));
            merge_package_json_object(grounding, obj);
        }
    }
}

fn select_npm_manifest(root: &Value, grounding: &mut SourceGrounding) -> Value {
    let latest = root
        .get("dist-tags")
        .and_then(Value::as_object)
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    if grounding.package_version.is_none() {
        grounding.package_version = latest.clone();
    }

    latest
        .as_deref()
        .and_then(|version| {
            root.get("versions")
                .and_then(Value::as_object)
                .and_then(|versions| versions.get(version))
        })
        .cloned()
        .unwrap_or_else(|| root.clone())
}

fn inspect_remote_pypi_package(grounding: &mut SourceGrounding) {
    if !remote_source_fetch_enabled() || grounding.inspected {
        return;
    }

    let Some(package_name) = grounding.package_name.as_deref() else {
        return;
    };
    let base = pypi_base_url();
    let endpoint = if let Some(version) = grounding.package_version.as_deref() {
        format!(
            "{}/pypi/{}/{}/json",
            base.trim_end_matches('/'),
            url_encode_path_segment(package_name),
            url_encode_path_segment(version)
        )
    } else {
        format!(
            "{}/pypi/{}/json",
            base.trim_end_matches('/'),
            url_encode_path_segment(package_name)
        )
    };

    let Some(root) = fetch_remote_json(&endpoint, Some("application/json")) else {
        return;
    };
    let Some(info) = root.get("info").and_then(Value::as_object) else {
        return;
    };

    record_source_inspection(grounding, None, Some(&endpoint));
    merge_pypi_info_object(grounding, info);
}

fn merge_pypi_info_object(grounding: &mut SourceGrounding, info: &serde_json::Map<String, Value>) {
    if grounding.package_name.is_none() {
        grounding.package_name = json_string(info.get("name"));
    }
    if grounding.package_version.is_none() {
        grounding.package_version = json_string(info.get("version"));
    }
    if grounding.homepage.is_none() {
        grounding.homepage =
            project_urls_value(info.get("project_urls"), &["Homepage", "homepage"])
                .or_else(|| json_string(info.get("home_page")));
    }
    if grounding.repository_url.is_none() {
        grounding.repository_url = project_urls_value(
            info.get("project_urls"),
            &[
                "Repository",
                "repository",
                "Source",
                "source",
                "Code",
                "code",
            ],
        )
        .or_else(|| json_string(info.get("project_url")));
    }
    if let Some(description) = json_string(info.get("description")) {
        record_derivation_evidence(
            grounding,
            DerivationEvidenceKind::RemoteDocSnippet,
            "pypi:description".to_string(),
            &description,
        );
    }
}

fn project_urls_value(value: Option<&Value>, keys: &[&str]) -> Option<String> {
    let urls = value?.as_object()?;
    keys.iter()
        .find_map(|key| urls.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn inspect_remote_repository_manifest(grounding: &mut SourceGrounding) {
    if !remote_source_fetch_enabled() {
        return;
    }
    if !needs_repository_manifest_inspection(grounding) {
        return;
    }

    let Some(repository_url) = grounding.repository_url.as_deref() else {
        return;
    };
    let Some((owner, repo)) = parse_github_repo(repository_url) else {
        return;
    };
    let base = github_api_base_url();

    for manifest in ["package.json", "pyproject.toml"] {
        let endpoint = format!(
            "{}/repos/{owner}/{repo}/contents/{manifest}",
            base.trim_end_matches('/')
        );
        let Some(raw) = fetch_github_contents(&endpoint) else {
            continue;
        };
        match manifest {
            "package.json" => {
                merge_package_json_text(grounding, &raw, None, Some(&endpoint));
            }
            "pyproject.toml" => {
                merge_pyproject_text(grounding, &raw, None, Some(&endpoint));
            }
            _ => {}
        }
        if grounding.inspected_urls.iter().any(|url| url == &endpoint) {
            break;
        }
    }

    for readme in ["README.md", "Readme.md", "readme.md"] {
        let endpoint = format!(
            "{}/repos/{owner}/{repo}/contents/{readme}",
            base.trim_end_matches('/')
        );
        let Some(raw) = fetch_github_contents(&endpoint) else {
            continue;
        };
        record_source_inspection(grounding, None, Some(&endpoint));
        record_derivation_evidence(
            grounding,
            DerivationEvidenceKind::ReadmeSnippet,
            endpoint,
            &raw,
        );
        break;
    }
}

fn needs_repository_manifest_inspection(grounding: &SourceGrounding) -> bool {
    grounding.repository_url.is_some()
        && (grounding.package_name.is_none()
            || grounding.package_version.is_none()
            || grounding.homepage.is_none()
            || !grounding.derivation_evidence.iter().any(|item| {
                matches!(
                    item.kind,
                    DerivationEvidenceKind::ReadmeSnippet
                        | DerivationEvidenceKind::RemoteDocSnippet
                        | DerivationEvidenceKind::CliHelp
                )
            }))
}

fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix("git+").unwrap_or(trimmed);
    let path = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))?;
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let owner = segments.next()?;
    let repo = segments.next()?;
    Some((owner.to_string(), repo.trim_end_matches(".git").to_string()))
}

fn npm_registry_base_url() -> String {
    std::env::var("MCPSMITH_NPM_REGISTRY_BASE_URL")
        .unwrap_or_else(|_| "https://registry.npmjs.org".to_string())
}

fn pypi_base_url() -> String {
    std::env::var("MCPSMITH_PYPI_BASE_URL").unwrap_or_else(|_| "https://pypi.org".to_string())
}

fn github_api_base_url() -> String {
    std::env::var("MCPSMITH_GITHUB_API_BASE_URL")
        .unwrap_or_else(|_| "https://api.github.com".to_string())
}

fn remote_source_fetch_enabled() -> bool {
    match std::env::var("MCPSMITH_SOURCE_FETCH") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => !cfg!(test),
    }
}

fn remote_source_fetch_timeout() -> Duration {
    let seconds = std::env::var("MCPSMITH_SOURCE_FETCH_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3);
    Duration::from_secs(seconds)
}

fn fetch_remote_json(url: &str, accept: Option<&str>) -> Option<Value> {
    let body = fetch_remote_text(url, accept)?;
    serde_json::from_str(&body).ok()
}

fn fetch_remote_text(url: &str, accept: Option<&str>) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(remote_source_fetch_timeout())
        .build();
    let mut request = agent.get(url).set("User-Agent", "mcpsmith");
    if let Some(accept) = accept {
        request = request.set("Accept", accept);
    }
    match request.call() {
        Ok(response) => response.into_string().ok(),
        Err(_) => None,
    }
}

fn fetch_github_contents(url: &str) -> Option<String> {
    let root = fetch_remote_json(url, Some("application/vnd.github+json"))?;
    let encoded = root.get("content")?.as_str()?;
    let normalized = encoded.lines().collect::<String>();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn url_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
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
    grounding.inspected_urls.sort();
    grounding.inspected_urls.dedup();
    grounding.derivation_evidence.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.excerpt.cmp(&right.excerpt))
    });
    grounding.derivation_evidence.dedup();

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
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};

    fn source_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<str>) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value.as_ref());
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                unsafe {
                    std::env::set_var(self.key, previous);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn spawn_http_server(routes: Vec<(&'static str, &'static str, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        std::thread::spawn(move || {
            for (expected_path, status, body) in routes {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                let first_line = request.lines().next().unwrap_or_default();
                assert!(
                    first_line.contains(expected_path),
                    "expected request path {expected_path}, got {first_line}"
                );
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });

        format!("http://{}", addr)
    }

    #[test]
    fn discover_resolves_npx_package_source_grounding() {
        let _guard = source_env_lock().lock().unwrap();
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
        let _guard = source_env_lock().lock().unwrap();
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
        let _guard = source_env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let tool_root = dir.path().join("local-tool");
        let bin_dir = tool_root.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let executable = bin_dir.join("server.sh");
        std::fs::write(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ] || [ \"$1\" = \"-h\" ]; then\n  echo 'Usage: server.sh --help'\n  exit 0\nfi\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&executable).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&executable, perms).unwrap();
        }
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
        std::fs::write(
            tool_root.join("README.md"),
            "# Local MCP\n\nUse `server.sh --help` to inspect available commands.\n",
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
        assert!(
            server
                .source_grounding
                .derivation_evidence
                .iter()
                .any(|item| {
                    item.kind == DerivationEvidenceKind::ManifestSnippet
                        && item.source.ends_with("package.json")
                })
        );
        assert!(
            server
                .source_grounding
                .derivation_evidence
                .iter()
                .any(|item| {
                    item.kind == DerivationEvidenceKind::ReadmeSnippet
                        && item.source.ends_with("README.md")
                })
        );
        assert!(
            server
                .source_grounding
                .derivation_evidence
                .iter()
                .any(|item| {
                    item.kind == DerivationEvidenceKind::CliHelp
                        && item.source.contains("server.sh --help")
                })
        );
    }

    #[test]
    fn discover_enriches_npm_package_from_remote_registry() {
        let _guard = source_env_lock().lock().unwrap();
        let readme = base64::engine::general_purpose::STANDARD
            .encode("# Playwright MCP\n\nUse the native browser tooling documented here.\n");
        let readme_body = format!(r#"{{"encoding":"base64","content":"{readme}"}}"#);
        let server = spawn_http_server(vec![
            (
                "/%40playwright%2Fmcp",
                "200 OK",
                r##"{"dist-tags":{"latest":"1.55.0"},"versions":{"1.55.0":{"name":"@playwright/mcp","version":"1.55.0","homepage":"https://playwright.dev","repository":{"url":"https://github.com/microsoft/playwright-mcp"},"readme":"# Playwright MCP\n\nUse the native browser tooling documented here.\n"}}}"##,
            ),
            (
                "/repos/microsoft/playwright-mcp/contents/README.md",
                "200 OK",
                Box::leak(readme_body.into_boxed_str()),
            ),
        ]);
        let _fetch = EnvVarGuard::set("MCPSMITH_SOURCE_FETCH", "1");
        let _registry = EnvVarGuard::set("MCPSMITH_NPM_REGISTRY_BASE_URL", &server);
        let _github = EnvVarGuard::set("MCPSMITH_GITHUB_API_BASE_URL", &server);

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp"]
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
            SourceEvidenceLevel::SourceInspected
        );
        assert_eq!(
            server.source_grounding.package_version.as_deref(),
            Some("1.55.0")
        );
        assert_eq!(
            server.source_grounding.homepage.as_deref(),
            Some("https://playwright.dev")
        );
        assert_eq!(
            server.source_grounding.repository_url.as_deref(),
            Some("https://github.com/microsoft/playwright-mcp")
        );
        assert!(
            server
                .source_grounding
                .inspected_urls
                .iter()
                .any(|url| { url.contains("/%40playwright%2Fmcp") })
        );
        assert!(
            server
                .source_grounding
                .derivation_evidence
                .iter()
                .any(|item| {
                    item.kind == DerivationEvidenceKind::ReadmeSnippet
                        && (item.source.contains("/%40playwright%2Fmcp")
                            || item
                                .source
                                .contains("/repos/microsoft/playwright-mcp/contents/README.md"))
                })
        );
    }

    #[test]
    fn discover_enriches_pypi_package_from_remote_registry() {
        let _guard = source_env_lock().lock().unwrap();
        let server = spawn_http_server(vec![(
            "/pypi/acme-memory-mcp/json",
            "200 OK",
            r#"{"info":{"name":"acme-memory-mcp","version":"0.4.1","project_urls":{"Homepage":"https://example.com/memory","Repository":"https://github.com/acme/memory-mcp"}}}"#,
        )]);
        let _fetch = EnvVarGuard::set("MCPSMITH_SOURCE_FETCH", "1");
        let _registry = EnvVarGuard::set("MCPSMITH_PYPI_BASE_URL", &server);

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "memory": {
      "command": "uvx",
      "args": ["acme-memory-mcp"]
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
            server.source_grounding.evidence_level,
            SourceEvidenceLevel::SourceInspected
        );
        assert_eq!(
            server.source_grounding.package_version.as_deref(),
            Some("0.4.1")
        );
        assert_eq!(
            server.source_grounding.homepage.as_deref(),
            Some("https://example.com/memory")
        );
        assert_eq!(
            server.source_grounding.repository_url.as_deref(),
            Some("https://github.com/acme/memory-mcp")
        );
    }

    #[test]
    fn discover_enriches_repository_url_from_remote_manifest() {
        let _guard = source_env_lock().lock().unwrap();
        let content = base64::engine::general_purpose::STANDARD.encode(
            r#"{"name":"repo-mcp","version":"2.0.0","homepage":"https://example.com/repo-mcp"}"#,
        );
        let body = format!(r#"{{"encoding":"base64","content":"{content}"}}"#);
        let readme = base64::engine::general_purpose::STANDARD
            .encode("# Repo MCP\n\nUse native commands described here.\n");
        let readme_body = format!(r#"{{"encoding":"base64","content":"{readme}"}}"#);
        let server = spawn_http_server(vec![
            (
                "/repos/acme/repo-mcp/contents/package.json",
                "200 OK",
                Box::leak(body.into_boxed_str()),
            ),
            (
                "/repos/acme/repo-mcp/contents/README.md",
                "200 OK",
                Box::leak(readme_body.into_boxed_str()),
            ),
        ]);
        let _fetch = EnvVarGuard::set("MCPSMITH_SOURCE_FETCH", "1");
        let _github = EnvVarGuard::set("MCPSMITH_GITHUB_API_BASE_URL", &server);

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("settings.json");
        std::fs::write(
            &config_path,
            r#"{
  "mcpServers": {
    "repo-backed": {
      "command": "custom-mcp",
      "repository": "https://github.com/acme/repo-mcp"
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
        assert_eq!(server.source_grounding.kind, SourceKind::RepositoryUrl);
        assert_eq!(
            server.source_grounding.evidence_level,
            SourceEvidenceLevel::SourceInspected
        );
        assert_eq!(
            server.source_grounding.package_name.as_deref(),
            Some("repo-mcp")
        );
        assert_eq!(
            server.source_grounding.package_version.as_deref(),
            Some("2.0.0")
        );
        assert_eq!(
            server.source_grounding.homepage.as_deref(),
            Some("https://example.com/repo-mcp")
        );
        assert!(
            server
                .source_grounding
                .inspected_urls
                .iter()
                .any(|url| url.contains("/repos/acme/repo-mcp/contents/package.json"))
        );
        assert!(
            server
                .source_grounding
                .derivation_evidence
                .iter()
                .any(|item| {
                    item.kind == DerivationEvidenceKind::ReadmeSnippet
                        && item
                            .source
                            .contains("/repos/acme/repo-mcp/contents/README.md")
                })
        );
    }
}
