use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConvertBackendPreference {
    #[default]
    Auto,
    Codex,
    Claude,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendConfig {
    #[serde(default)]
    pub preference: ConvertBackendPreference,
    #[serde(default = "default_convert_backend_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_convert_backend_chunk_size")]
    pub chunk_size: usize,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            preference: ConvertBackendPreference::Auto,
            timeout_seconds: 90,
            chunk_size: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeConfig {
    #[serde(default = "default_convert_probe_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_convert_probe_retries")]
    pub retries: u32,
    #[serde(default)]
    pub allow_side_effects: bool,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            retries: 0,
            allow_side_effects: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
struct RawBackendConfig {
    preference: Option<ConvertBackendPreference>,
    timeout_seconds: Option<u64>,
    chunk_size: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
struct RawProbeConfig {
    timeout_seconds: Option<u64>,
    retries: Option<u32>,
    allow_side_effects: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
struct LegacyConvertConfig {
    backend_preference: Option<ConvertBackendPreference>,
    backend_timeout_seconds: Option<u64>,
    backend_chunk_size: Option<usize>,
    probe_timeout_seconds: Option<u64>,
    probe_retries: Option<u32>,
    allow_side_effect_probes: Option<bool>,
}

fn default_convert_backend_timeout_seconds() -> u64 {
    90
}

fn default_convert_backend_chunk_size() -> usize {
    8
}

fn default_convert_probe_timeout_seconds() -> u64 {
    30
}

fn default_convert_probe_retries() -> u32 {
    0
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub probe: ProbeConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
struct RawConfig {
    backend: Option<RawBackendConfig>,
    probe: Option<RawProbeConfig>,
    convert: Option<LegacyConvertConfig>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawConfig::deserialize(deserializer)?;
        let backend = raw.backend.unwrap_or_default();
        let probe = raw.probe.unwrap_or_default();
        let legacy = raw.convert.unwrap_or_default();
        let backend_defaults = BackendConfig::default();
        let probe_defaults = ProbeConfig::default();

        Ok(Config {
            backend: BackendConfig {
                preference: backend
                    .preference
                    .or(legacy.backend_preference)
                    .unwrap_or(backend_defaults.preference),
                timeout_seconds: backend
                    .timeout_seconds
                    .or(legacy.backend_timeout_seconds)
                    .unwrap_or(backend_defaults.timeout_seconds),
                chunk_size: backend
                    .chunk_size
                    .or(legacy.backend_chunk_size)
                    .unwrap_or(backend_defaults.chunk_size),
            },
            probe: ProbeConfig {
                timeout_seconds: probe
                    .timeout_seconds
                    .or(legacy.probe_timeout_seconds)
                    .unwrap_or(probe_defaults.timeout_seconds),
                retries: probe
                    .retries
                    .or(legacy.probe_retries)
                    .unwrap_or(probe_defaults.retries),
                allow_side_effects: probe
                    .allow_side_effects
                    .or(legacy.allow_side_effect_probes)
                    .unwrap_or(probe_defaults.allow_side_effects),
            },
        })
    }
}

impl Config {
    pub fn base_dir() -> PathBuf {
        home_dir().join(".mcpsmith")
    }

    pub fn config_path() -> PathBuf {
        Self::base_dir().join("config.yaml")
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Config =
            serde_yaml::from_str(&contents).with_context(|| "Failed to parse config")?;
        Ok(config)
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.backend.preference, ConvertBackendPreference::Auto);
        assert_eq!(config.backend.timeout_seconds, 90);
        assert_eq!(config.backend.chunk_size, 8);
        assert_eq!(config.probe.timeout_seconds, 30);
        assert_eq!(config.probe.retries, 0);
        assert!(!config.probe.allow_side_effects);
    }

    #[test]
    fn test_config_roundtrip_yaml() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_config_reads_canonical_backend_and_probe_keys() {
        let parsed: Config = serde_yaml::from_str(
            r#"
backend:
  preference: claude
  timeout_seconds: 12
  chunk_size: 3
probe:
  timeout_seconds: 44
  retries: 2
  allow_side_effects: true
"#,
        )
        .unwrap();

        assert_eq!(parsed.backend.preference, ConvertBackendPreference::Claude);
        assert_eq!(parsed.backend.timeout_seconds, 12);
        assert_eq!(parsed.backend.chunk_size, 3);
        assert_eq!(parsed.probe.timeout_seconds, 44);
        assert_eq!(parsed.probe.retries, 2);
        assert!(parsed.probe.allow_side_effects);
    }

    #[test]
    fn test_config_reads_legacy_convert_keys_for_compatibility() {
        let parsed: Config = serde_yaml::from_str(
            r#"
convert:
  backend_preference: codex
  backend_timeout_seconds: 17
  backend_chunk_size: 4
  probe_timeout_seconds: 55
  probe_retries: 5
  allow_side_effect_probes: true
"#,
        )
        .unwrap();

        assert_eq!(parsed.backend.preference, ConvertBackendPreference::Codex);
        assert_eq!(parsed.backend.timeout_seconds, 17);
        assert_eq!(parsed.backend.chunk_size, 4);
        assert_eq!(parsed.probe.timeout_seconds, 55);
        assert_eq!(parsed.probe.retries, 5);
        assert!(parsed.probe.allow_side_effects);
    }

    #[test]
    fn test_canonical_config_keys_override_legacy_values_field_by_field() {
        let parsed: Config = serde_yaml::from_str(
            r#"
backend:
  timeout_seconds: 12
probe:
  retries: 2
convert:
  backend_preference: codex
  backend_timeout_seconds: 17
  backend_chunk_size: 4
  probe_timeout_seconds: 55
  probe_retries: 5
  allow_side_effect_probes: true
"#,
        )
        .unwrap();

        assert_eq!(parsed.backend.preference, ConvertBackendPreference::Codex);
        assert_eq!(parsed.backend.timeout_seconds, 12);
        assert_eq!(parsed.backend.chunk_size, 4);
        assert_eq!(parsed.probe.timeout_seconds, 55);
        assert_eq!(parsed.probe.retries, 2);
        assert!(parsed.probe.allow_side_effects);
    }
}
