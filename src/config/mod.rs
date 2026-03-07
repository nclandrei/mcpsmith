use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
pub struct ConvertConfig {
    #[serde(default)]
    pub backend_preference: ConvertBackendPreference,
    #[serde(default = "default_convert_backend_timeout_seconds")]
    pub backend_timeout_seconds: u64,
    #[serde(default = "default_convert_backend_chunk_size")]
    pub backend_chunk_size: usize,
    #[serde(default = "default_convert_probe_timeout_seconds")]
    pub probe_timeout_seconds: u64,
    #[serde(default = "default_convert_probe_retries")]
    pub probe_retries: u32,
    #[serde(default)]
    pub allow_side_effect_probes: bool,
}

impl Default for ConvertConfig {
    fn default() -> Self {
        Self {
            backend_preference: ConvertBackendPreference::Auto,
            backend_timeout_seconds: 90,
            backend_chunk_size: 8,
            probe_timeout_seconds: 30,
            probe_retries: 0,
            allow_side_effect_probes: false,
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Config {
    #[serde(default)]
    pub convert: ConvertConfig,
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
        assert_eq!(
            config.convert.backend_preference,
            ConvertBackendPreference::Auto
        );
        assert_eq!(config.convert.backend_timeout_seconds, 90);
        assert_eq!(config.convert.backend_chunk_size, 8);
        assert_eq!(config.convert.probe_timeout_seconds, 30);
        assert_eq!(config.convert.probe_retries, 0);
        assert!(!config.convert.allow_side_effect_probes);
    }

    #[test]
    fn test_config_roundtrip_yaml() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config, parsed);
    }
}
