//! Configuration loading for Merlion Agent.
//!
//! Hierarchy (later overrides earlier):
//! 1. compiled-in defaults
//! 2. `~/.merlion/config.yaml`
//! 3. project `./.merlion/config.yaml` (if present)
//! 4. environment variables (`MERLION_*`)
//!
//! Secrets live in `~/.merlion/.env`, loaded once at startup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub model: ModelConfig,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_iterations() -> u32 {
    32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// e.g. "openai:gpt-4o-mini", "openrouter:anthropic/claude-sonnet-4",
    /// "nous:Hermes-4-405B". Resolved by [`Config::resolve_provider`].
    pub id: String,
    /// Base URL override. If unset, picked from the provider prefix.
    #[serde(default)]
    pub base_url: Option<String>,
    /// API-key env-var name (e.g. "OPENAI_API_KEY"). If unset, picked from
    /// the provider prefix.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: ModelConfig {
                id: "openai:gpt-4o-mini".into(),
                base_url: None,
                api_key_env: None,
                temperature: None,
                max_tokens: None,
            },
            system_prompt: None,
            max_iterations: default_max_iterations(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// `POST /chat/completions` with `Authorization: Bearer <key>`.
    OpenAi,
    /// `POST /messages` with `x-api-key: <key>` and `anthropic-version` header.
    Anthropic,
}

pub struct ResolvedProvider {
    pub model: String,
    pub base_url: String,
    pub api_key_env: String,
    pub wire: Wire,
}

impl Config {
    pub fn resolve_provider(&self) -> Result<ResolvedProvider> {
        let (provider, model) = match self.model.id.split_once(':') {
            Some((p, m)) => (p, m),
            None => ("openai", self.model.id.as_str()),
        };
        let (default_base, default_env, wire) = match provider {
            "openai" => ("https://api.openai.com/v1", "OPENAI_API_KEY", Wire::OpenAi),
            "openrouter" => ("https://openrouter.ai/api/v1", "OPENROUTER_API_KEY", Wire::OpenAi),
            "nous" => ("https://inference-api.nousresearch.com/v1", "NOUS_API_KEY", Wire::OpenAi),
            "novita" => ("https://api.novita.ai/v3/openai", "NOVITA_API_KEY", Wire::OpenAi),
            "moonshot" => ("https://api.moonshot.ai/v1", "MOONSHOT_API_KEY", Wire::OpenAi),
            "minimax" => ("https://api.minimaxi.chat/v1", "MINIMAX_API_KEY", Wire::OpenAi),
            "zai" | "glm" => ("https://api.z.ai/api/paas/v4", "ZAI_API_KEY", Wire::OpenAi),
            "groq" => ("https://api.groq.com/openai/v1", "GROQ_API_KEY", Wire::OpenAi),
            "deepseek" => ("https://api.deepseek.com/v1", "DEEPSEEK_API_KEY", Wire::OpenAi),
            "anthropic" => ("https://api.anthropic.com/v1", "ANTHROPIC_API_KEY", Wire::Anthropic),
            other => {
                anyhow::bail!(
                    "unknown provider `{other}`. Set `model.base_url` and `model.api_key_env` explicitly, \
                     or use one of: openai, openrouter, nous, novita, moonshot, minimax, zai, groq, deepseek, anthropic."
                );
            }
        };
        Ok(ResolvedProvider {
            model: model.to_string(),
            base_url: self.model.base_url.clone().unwrap_or_else(|| default_base.to_string()),
            api_key_env: self.model.api_key_env.clone().unwrap_or_else(|| default_env.to_string()),
            wire,
        })
    }
}

pub fn merlion_home() -> PathBuf {
    if let Ok(p) = std::env::var("MERLION_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir().map(|h| h.join(".merlion")).unwrap_or_else(|| PathBuf::from(".merlion"))
}

pub fn ensure_home() -> Result<PathBuf> {
    let home = merlion_home();
    std::fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    Ok(home)
}

pub fn load() -> Result<Config> {
    let home = ensure_home()?;
    let _ = dotenvy::from_path(home.join(".env"));
    let _ = dotenvy::from_path(".env");

    let mut cfg = Config::default();
    let user_cfg = home.join("config.yaml");
    if user_cfg.exists() {
        merge_yaml(&mut cfg, &user_cfg)?;
    }
    let project_cfg = PathBuf::from(".merlion/config.yaml");
    if project_cfg.exists() {
        merge_yaml(&mut cfg, &project_cfg)?;
    }
    apply_env_overrides(&mut cfg);
    Ok(cfg)
}

fn merge_yaml(cfg: &mut Config, path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let from_file: Config =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    *cfg = from_file;
    Ok(())
}

fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(v) = std::env::var("MERLION_MODEL") {
        cfg.model.id = v;
    }
    if let Ok(v) = std::env::var("MERLION_BASE_URL") {
        cfg.model.base_url = Some(v);
    }
    if let Ok(v) = std::env::var("MERLION_API_KEY_ENV") {
        cfg.model.api_key_env = Some(v);
    }
    if let Ok(v) = std::env::var("MERLION_SYSTEM_PROMPT") {
        cfg.system_prompt = Some(v);
    }
    if let Ok(v) = std::env::var("MERLION_MAX_ITERATIONS") {
        if let Ok(n) = v.parse() {
            cfg.max_iterations = n;
        }
    }
}

pub fn save(cfg: &Config) -> Result<PathBuf> {
    let home = ensure_home()?;
    let path = home.join("config.yaml");
    let yaml = serde_yaml::to_string(cfg)?;
    std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}
