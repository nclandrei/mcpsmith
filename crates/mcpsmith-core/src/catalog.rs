use crate::SourceKind;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const OFFICIAL_REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io/v0.1";
const SMITHERY_REGISTRY_BASE: &str = "https://registry.smithery.ai";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogProvider {
    Official,
    Smithery,
    Glama,
}

impl fmt::Display for CatalogProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogProvider::Official => write!(f, "official"),
            CatalogProvider::Smithery => write!(f, "smithery"),
            CatalogProvider::Glama => write!(f, "glama"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogSyncOptions {
    #[serde(default = "default_catalog_providers")]
    pub providers: Vec<CatalogProvider>,
    #[serde(default)]
    pub cache_root: Option<PathBuf>,
}

impl Default for CatalogSyncOptions {
    fn default() -> Self {
        Self {
            providers: default_catalog_providers(),
            cache_root: None,
        }
    }
}

fn default_catalog_providers() -> Vec<CatalogProvider> {
    vec![
        CatalogProvider::Official,
        CatalogProvider::Smithery,
        CatalogProvider::Glama,
    ]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogSourceResolutionStatus {
    Resolvable,
    RemoteOnly,
    Unresolved,
    UnsupportedProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogSourceResolution {
    pub status: CatalogSourceResolutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogProviderRecord {
    pub provider: CatalogProvider,
    pub provider_id: String,
    pub canonical_name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_manager: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub remote: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogServer {
    pub dedupe_key: String,
    pub canonical_name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub source_resolution: CatalogSourceResolution,
    pub provider_records: Vec<CatalogProviderRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogProviderStatus {
    pub provider: CatalogProvider,
    pub supported: bool,
    pub record_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_capture_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogStats {
    pub provider_record_counts: BTreeMap<String, usize>,
    pub unique_servers: usize,
    pub source_resolvable: usize,
    pub remote_only: usize,
    pub unresolved: usize,
    pub unsupported_provider_records: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogSyncResult {
    pub generated_at: DateTime<Utc>,
    pub cache_root: PathBuf,
    pub providers: Vec<CatalogProviderStatus>,
    pub servers: Vec<CatalogServer>,
    pub stats: CatalogStats,
}

pub fn load_catalog_sync_result(path: &Path) -> Result<CatalogSyncResult> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read catalog sync result {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse catalog sync result {}", path.display()))
}

pub fn load_cached_catalog_sync_result(cache_root: Option<PathBuf>) -> Result<CatalogSyncResult> {
    let root = cache_root.unwrap_or_else(default_catalog_cache_root);
    load_catalog_sync_result(&root.join("latest.json"))
}

pub fn catalog_stats(result: &CatalogSyncResult) -> CatalogStats {
    result.stats.clone()
}

pub fn catalog_sync(options: &CatalogSyncOptions) -> Result<CatalogSyncResult> {
    let cache_root = options
        .cache_root
        .clone()
        .unwrap_or_else(default_catalog_cache_root);
    fs::create_dir_all(&cache_root)
        .with_context(|| format!("Failed to create {}", cache_root.display()))?;

    let mut statuses = Vec::new();
    let mut records = Vec::new();

    for provider in dedupe_providers(&options.providers) {
        match provider {
            CatalogProvider::Official => {
                let (status, fetched) = fetch_official_records(&cache_root)?;
                statuses.push(status);
                records.extend(fetched);
            }
            CatalogProvider::Smithery => {
                let (status, fetched) = fetch_smithery_records(&cache_root)?;
                statuses.push(status);
                records.extend(fetched);
            }
            CatalogProvider::Glama => {
                statuses.push(CatalogProviderStatus {
                    provider,
                    supported: false,
                    record_count: 0,
                    raw_capture_path: None,
                    diagnostics: vec![
                        "Glama is excluded from exact census because no stable machine-readable list API/export is configured.".to_string(),
                    ],
                });
            }
        }
    }

    let servers = dedupe_catalog_records(records);
    let stats = build_catalog_stats(&statuses, &servers);

    let result = CatalogSyncResult {
        generated_at: Utc::now(),
        cache_root,
        providers: statuses,
        servers,
        stats,
    };
    let latest = result.cache_root.join("latest.json");
    let body =
        serde_json::to_string_pretty(&result).context("Failed to serialize catalog snapshot")?;
    fs::write(&latest, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", latest.display()))?;
    Ok(result)
}

fn dedupe_providers(providers: &[CatalogProvider]) -> Vec<CatalogProvider> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for provider in providers {
        if seen.insert(*provider) {
            out.push(*provider);
        }
    }
    out
}

fn default_catalog_cache_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".mcpsmith")
        .join("cache")
        .join("catalog")
}

fn fetch_official_records(
    cache_root: &Path,
) -> Result<(CatalogProviderStatus, Vec<CatalogProviderRecord>)> {
    let mut cursor: Option<String> = None;
    let mut pages = Vec::<Value>::new();
    let mut records = Vec::new();

    loop {
        let mut url = format!("{}/servers?limit=200", official_registry_base());
        if let Some(cursor) = cursor.as_deref() {
            url.push_str("&cursor=");
            url.push_str(&url_encode_path_segment(cursor));
        }

        let page = fetch_json(&url)?;
        let next_cursor = page
            .get("metadata")
            .and_then(|v| v.get("nextCursor"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let entries = page
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in &entries {
            if let Some(record) = normalize_official_record(entry) {
                records.push(record);
            }
        }
        pages.push(page);

        if next_cursor.is_none() {
            break;
        }
        cursor = next_cursor;
    }

    let capture_path = cache_root.join("official-registry.raw.json");
    write_json(&capture_path, &Value::Array(pages))?;
    Ok((
        CatalogProviderStatus {
            provider: CatalogProvider::Official,
            supported: true,
            record_count: records.len(),
            raw_capture_path: Some(capture_path),
            diagnostics: vec![],
        },
        records,
    ))
}

fn fetch_smithery_records(
    cache_root: &Path,
) -> Result<(CatalogProviderStatus, Vec<CatalogProviderRecord>)> {
    let mut page_number = 1usize;
    let mut total_pages = 1usize;
    let mut pages = Vec::<Value>::new();
    let mut records = Vec::new();

    while page_number <= total_pages {
        let url = format!(
            "{}/servers?pageSize=200&page={page_number}",
            smithery_registry_base()
        );
        let page = fetch_json(&url)?;
        total_pages = page
            .get("pagination")
            .and_then(|v| v.get("totalPages"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let entries = page
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in &entries {
            if let Some(record) = normalize_smithery_record(entry) {
                records.push(record);
            }
        }
        pages.push(page);
        page_number += 1;
    }

    let capture_path = cache_root.join("smithery-registry.raw.json");
    write_json(&capture_path, &Value::Array(pages))?;
    Ok((
        CatalogProviderStatus {
            provider: CatalogProvider::Smithery,
            supported: true,
            record_count: records.len(),
            raw_capture_path: Some(capture_path),
            diagnostics: vec![],
        },
        records,
    ))
}

fn fetch_json(url: &str) -> Result<Value> {
    let response = ureq::get(url)
        .set("User-Agent", "mcpsmith")
        .call()
        .with_context(|| format!("Failed to fetch {url}"))?;
    response
        .into_json::<Value>()
        .with_context(|| format!("Failed to parse JSON from {url}"))
}

fn official_registry_base() -> String {
    std::env::var("MCPSMITH_OFFICIAL_REGISTRY_BASE_URL")
        .unwrap_or_else(|_| OFFICIAL_REGISTRY_BASE.to_string())
}

fn smithery_registry_base() -> String {
    std::env::var("MCPSMITH_SMITHERY_REGISTRY_BASE_URL")
        .unwrap_or_else(|_| SMITHERY_REGISTRY_BASE.to_string())
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let body = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;
    fs::write(path, format!("{body}\n"))
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn normalize_official_record(entry: &Value) -> Option<CatalogProviderRecord> {
    let server = entry.get("server")?.as_object()?;
    let canonical_name = server.get("name")?.as_str()?.trim().to_string();
    let display_name = server
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| server.get("name").and_then(Value::as_str))
        .unwrap_or(&canonical_name)
        .trim()
        .to_string();
    let package = extract_package_metadata(server.get("packages"));
    let repository_url = server
        .get("repository")
        .and_then(|value| value.get("url"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let remote_url = server
        .get("remotes")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("url")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
        });
    let version = server
        .get("version")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    Some(CatalogProviderRecord {
        provider: CatalogProvider::Official,
        provider_id: if let Some(version) = version.as_deref() {
            format!("{canonical_name}:{version}")
        } else {
            canonical_name.clone()
        },
        canonical_name: canonical_name.clone(),
        display_name,
        description: server
            .get("description")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        homepage: server
            .get("websiteUrl")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        repository_url,
        package_manager: package.0,
        package_name: package.1,
        package_version: version.or(package.2),
        remote_url,
        remote: server
            .get("remotes")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()),
        aliases: vec![],
    })
}

fn normalize_smithery_record(entry: &Value) -> Option<CatalogProviderRecord> {
    let qualified_name = entry.get("qualifiedName")?.as_str()?.trim().to_string();
    let display_name = entry
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or(&qualified_name)
        .trim()
        .to_string();
    let namespace = entry
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let slug = entry
        .get("slug")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let mut aliases = vec![];
    if let Some(namespace) = namespace.as_deref()
        && let Some(slug) = slug.as_deref()
    {
        aliases.push(format!("{namespace}/{slug}"));
    }
    if let Some(slug) = slug {
        aliases.push(slug);
    }

    Some(CatalogProviderRecord {
        provider: CatalogProvider::Smithery,
        provider_id: qualified_name.clone(),
        canonical_name: qualified_name.clone(),
        display_name,
        description: entry
            .get("description")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        homepage: entry
            .get("homepage")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        repository_url: None,
        package_manager: None,
        package_name: None,
        package_version: None,
        remote_url: None,
        remote: entry
            .get("remote")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        aliases,
    })
}

fn extract_package_metadata(
    value: Option<&Value>,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(value) = value else {
        return (None, None, None);
    };

    if let Some(array) = value.as_array() {
        for item in array {
            let kind = item
                .get("registryType")
                .or_else(|| item.get("type"))
                .and_then(Value::as_str)
                .map(|value| value.to_ascii_lowercase());
            let name = item
                .get("identifier")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let version = item
                .get("version")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if let (Some(kind), Some(name)) = (kind, name) {
                return (Some(kind), Some(name), version);
            }
        }
    }

    if let Some(object) = value.as_object() {
        for (kind, entry) in object {
            if let Some(obj) = entry.as_object() {
                let name = obj
                    .get("identifier")
                    .or_else(|| obj.get("name"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let version = obj
                    .get("version")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if let Some(name) = name {
                    return (Some(kind.to_ascii_lowercase()), Some(name), version);
                }
            }
        }
    }

    (None, None, None)
}

fn dedupe_catalog_records(records: Vec<CatalogProviderRecord>) -> Vec<CatalogServer> {
    let mut grouped = BTreeMap::<String, Vec<CatalogProviderRecord>>::new();
    for record in records {
        let resolution = resolve_catalog_record(&record);
        let key = resolution
            .identity
            .clone()
            .or_else(|| resolution.source_url.clone())
            .unwrap_or_else(|| record.canonical_name.to_ascii_lowercase());
        grouped.entry(key).or_default().push(record);
    }

    grouped
        .into_iter()
        .map(|(dedupe_key, mut records)| {
            records.sort_by(|left, right| {
                left.provider
                    .cmp(&right.provider)
                    .then_with(|| left.provider_id.cmp(&right.provider_id))
            });
            let primary = records
                .iter()
                .find(|record| record.provider == CatalogProvider::Official)
                .unwrap_or(&records[0]);
            let mut aliases = records
                .iter()
                .flat_map(|record| {
                    std::iter::once(record.canonical_name.clone())
                        .chain(std::iter::once(record.display_name.clone()))
                        .chain(record.aliases.clone())
                })
                .collect::<Vec<_>>();
            aliases.sort();
            aliases.dedup();

            let resolution = records
                .iter()
                .map(resolve_catalog_record)
                .find(|resolution| resolution.status == CatalogSourceResolutionStatus::Resolvable)
                .or_else(|| {
                    records
                        .iter()
                        .map(resolve_catalog_record)
                        .find(|resolution| {
                            resolution.status == CatalogSourceResolutionStatus::RemoteOnly
                        })
                })
                .unwrap_or_else(|| resolve_catalog_record(primary));

            CatalogServer {
                dedupe_key,
                canonical_name: primary.canonical_name.clone(),
                display_name: primary.display_name.clone(),
                aliases,
                source_resolution: resolution,
                provider_records: records,
            }
        })
        .collect()
}

fn resolve_catalog_record(record: &CatalogProviderRecord) -> CatalogSourceResolution {
    if record.provider == CatalogProvider::Glama {
        return CatalogSourceResolution {
            status: CatalogSourceResolutionStatus::UnsupportedProvider,
            kind: None,
            identity: None,
            source_url: None,
            diagnostics: vec!["Provider excluded from exact census.".to_string()],
        };
    }

    if let Some(repository_url) = record.repository_url.as_deref() {
        return CatalogSourceResolution {
            status: CatalogSourceResolutionStatus::Resolvable,
            kind: Some(SourceKind::RepositoryUrl),
            identity: Some(repository_url.to_string()),
            source_url: Some(repository_url.to_string()),
            diagnostics: vec![],
        };
    }

    if let (Some(package_manager), Some(package_name)) = (
        record.package_manager.as_deref(),
        record.package_name.as_deref(),
    ) {
        let kind = match package_manager {
            "npm" => Some(SourceKind::NpmPackage),
            "pypi" => Some(SourceKind::PypiPackage),
            _ => None,
        };
        if let Some(kind) = kind {
            return CatalogSourceResolution {
                status: CatalogSourceResolutionStatus::Resolvable,
                kind: Some(kind),
                identity: Some(match record.package_version.as_deref() {
                    Some(version) => format!("{package_name}@{version}"),
                    None => package_name.to_string(),
                }),
                source_url: None,
                diagnostics: vec![],
            };
        }
    }

    if record.remote || record.remote_url.is_some() {
        return CatalogSourceResolution {
            status: CatalogSourceResolutionStatus::RemoteOnly,
            kind: Some(SourceKind::RemoteUrl),
            identity: record
                .remote_url
                .clone()
                .or_else(|| Some(record.provider_id.clone())),
            source_url: record.remote_url.clone(),
            diagnostics: vec![
                "Listing exposes a remote deployment but no installable source artifact."
                    .to_string(),
            ],
        };
    }

    CatalogSourceResolution {
        status: CatalogSourceResolutionStatus::Unresolved,
        kind: None,
        identity: Some(record.provider_id.clone()),
        source_url: None,
        diagnostics: vec![
            "No repository or package artifact was exposed by the provider record.".to_string(),
        ],
    }
}

fn build_catalog_stats(
    statuses: &[CatalogProviderStatus],
    servers: &[CatalogServer],
) -> CatalogStats {
    let provider_record_counts = statuses
        .iter()
        .map(|status| (status.provider.to_string(), status.record_count))
        .collect::<BTreeMap<_, _>>();

    let mut source_resolvable = 0usize;
    let mut remote_only = 0usize;
    let mut unresolved = 0usize;
    let mut unsupported_provider_records = 0usize;

    for server in servers {
        match server.source_resolution.status {
            CatalogSourceResolutionStatus::Resolvable => source_resolvable += 1,
            CatalogSourceResolutionStatus::RemoteOnly => remote_only += 1,
            CatalogSourceResolutionStatus::Unresolved => unresolved += 1,
            CatalogSourceResolutionStatus::UnsupportedProvider => unsupported_provider_records += 1,
        }
    }

    CatalogStats {
        provider_record_counts,
        unique_servers: servers.len(),
        source_resolvable,
        remote_only,
        unresolved,
        unsupported_provider_records,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_catalog_record_prefers_repository_url() {
        let record = CatalogProviderRecord {
            provider: CatalogProvider::Official,
            provider_id: "demo:1.0.0".to_string(),
            canonical_name: "demo".to_string(),
            display_name: "Demo".to_string(),
            description: None,
            homepage: None,
            repository_url: Some("https://github.com/acme/demo".to_string()),
            package_manager: Some("npm".to_string()),
            package_name: Some("@acme/demo".to_string()),
            package_version: Some("1.0.0".to_string()),
            remote_url: None,
            remote: false,
            aliases: vec![],
        };

        let resolution = resolve_catalog_record(&record);
        assert_eq!(resolution.status, CatalogSourceResolutionStatus::Resolvable);
        assert_eq!(resolution.kind, Some(SourceKind::RepositoryUrl));
    }

    #[test]
    fn build_catalog_stats_counts_statuses() {
        let server = CatalogServer {
            dedupe_key: "demo".to_string(),
            canonical_name: "demo".to_string(),
            display_name: "Demo".to_string(),
            aliases: vec![],
            source_resolution: CatalogSourceResolution {
                status: CatalogSourceResolutionStatus::RemoteOnly,
                kind: Some(SourceKind::RemoteUrl),
                identity: None,
                source_url: None,
                diagnostics: vec![],
            },
            provider_records: vec![],
        };
        let stats = build_catalog_stats(
            &[CatalogProviderStatus {
                provider: CatalogProvider::Official,
                supported: true,
                record_count: 3,
                raw_capture_path: None,
                diagnostics: vec![],
            }],
            &[server],
        );

        assert_eq!(stats.unique_servers, 1);
        assert_eq!(stats.remote_only, 1);
        assert_eq!(stats.provider_record_counts["official"], 3);
    }
}
