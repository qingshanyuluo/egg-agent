use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Persisted configuration for egg-agent.
///
/// The provider speaks the OpenAI Chat Completions protocol, so any
/// OpenAI-compatible endpoint (OpenAI, DeepSeek, Kimi, GLM, Qwen, a local
/// llama.cpp server, …) works by pointing `base_url` and `model` at it.
///
/// Stored as TOML at `~/.egg-agent/config.toml`. Environment variables
/// (`EGG_API_KEY` / `EGG_BASE_URL` / `EGG_MODEL`) override the file when set,
/// which is handy for one-off runs and CI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_key: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Optional auxiliary model for lightweight tasks (translation, tool
    /// explanation, memory consolidation). When absent, aux features are
    /// silently disabled. `base_url` and `api_key` default to the main
    /// config when omitted.
    #[serde(default)]
    pub aux: Option<AuxConfig>,
}

/// Configuration for a secondary "auxiliary" model used by plugins.
///
/// The aux model should be cheap and fast (e.g. Qwen-2.5-7B on the same
/// provider). It handles non-critical background tasks so the main model
/// isn't wasted on translation/explanation overhead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuxConfig {
    /// Model ID for aux tasks. An empty string means "aux not configured".
    #[serde(default)]
    pub model: String,
    /// Base URL for the aux provider. Falls back to the main `base_url`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// API key for the aux provider. Falls back to the main `api_key`.
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_model() -> String {
    "gpt-4o-mini".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: default_base_url(),
            model: default_model(),
            aux: None,
        }
    }
}

impl Config {
    /// Directory holding the config file: `~/.egg-agent/`.
    pub fn dir() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".egg-agent"))
    }

    /// Full path to the config file: `~/.egg-agent/config.toml`.
    pub fn path() -> Result<PathBuf> {
        Ok(Self::dir()?.join("config.toml"))
    }

    /// Load config from disk, then let environment variables override.
    ///
    /// Returns a friendly error (pointing at `egg config`) if nothing is
    /// configured — no file and no `EGG_API_KEY`.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let mut config = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("could not read config at {}", path.display()))?;
            toml::from_str(&text)
                .with_context(|| format!("could not parse config at {}", path.display()))?
        } else {
            Config::default()
        };

        // Env overrides (any subset).
        if let Ok(v) = env::var("EGG_API_KEY") {
            config.api_key = v;
        }
        if let Ok(v) = env::var("EGG_BASE_URL") {
            config.base_url = v;
        }
        if let Ok(v) = env::var("EGG_MODEL") {
            config.model = v;
        }

        config.base_url = config.base_url.trim_end_matches('/').to_string();

        // An aux section with an empty model means "not configured".
        if config.aux.as_ref().map_or(false, |a| a.model.trim().is_empty()) {
            config.aux = None;
        }

        if config.api_key.trim().is_empty() {
            bail!(
                "no API key configured.\n  Run `egg config` to set it up, \
                 or set the EGG_API_KEY environment variable."
            );
        }

        Ok(config)
    }

    /// Load config from disk only (no env override, no api_key requirement).
    /// Used by the `config` wizard to pre-fill current values.
    pub fn load_file_or_default() -> Result<Self> {
        let path = Self::path()?;
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("could not read config at {}", path.display()))?;
            let cfg = toml::from_str(&text)
                .with_context(|| format!("could not parse config at {}", path.display()))?;
            Ok(cfg)
        } else {
            Ok(Config::default())
        }
    }

    /// Write config to `~/.egg-agent/config.toml`, creating the dir if needed.
    pub fn save(&self) -> Result<PathBuf> {
        let dir = Self::dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create config dir {}", dir.display()))?;
        let path = Self::path()?;
        let text = toml::to_string_pretty(self).context("could not serialize config")?;
        std::fs::write(&path, text)
            .with_context(|| format!("could not write config at {}", path.display()))?;

        // Best-effort: tighten permissions since the file holds an API key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip() {
        let cfg = Config {
            api_key: "sk-abc".to_string(),
            base_url: "https://example.com/v1".to_string(),
            model: "some-model".to_string(),
            aux: None,
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.api_key, "sk-abc");
        assert_eq!(back.base_url, "https://example.com/v1");
        assert_eq!(back.model, "some-model");
    }

    #[test]
    fn defaults_fill_missing_fields() {
        // Only api_key present -> base_url/model come from defaults.
        let cfg: Config = toml::from_str(r#"api_key = "sk-xyz""#).unwrap();
        assert_eq!(cfg.api_key, "sk-xyz");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert!(cfg.aux.is_none());
    }
}
