//! Configuration loading for Merlion Agent.
//!
//! Hierarchy (later overrides earlier):
//! 1. compiled-in defaults
//! 2. `~/.merlion/config.yaml`
//! 3. project `./.merlion/config.yaml` (if present)
//! 4. environment variables (`MERLION_*`)
//!
//! Secrets live in `~/.merlion/.env`, loaded once at startup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub model: ModelConfig,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub hooks: Hooks,
}

/// Shell-script hooks invoked at agent lifecycle events. Each hook is
/// a shell command run with the hook's payload piped to stdin.
#[derive(Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Hooks {
    /// Run before any tool dispatch. stdin: JSON { "tool": "...", "args": {...} }.
    pub before_tool: Vec<String>,
    /// Run after a tool returns. stdin: JSON { "tool": "...", "result": "..." }.
    pub after_tool: Vec<String>,
    /// Run when a chat session starts. stdin: JSON { "session_id": "..." }.
    pub session_start: Vec<String>,
    /// Run on chat session end. stdin: JSON { "session_id": "...", "messages": N }.
    pub session_end: Vec<String>,
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
            hooks: Hooks::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// `POST /chat/completions` with `Authorization: Bearer <key>`.
    OpenAi,
    /// `POST /messages` with `x-api-key: <key>` and `anthropic-version` header.
    Anthropic,
    /// `POST /models/<m>:streamGenerateContent?alt=sse` with `x-goog-api-key: <key>`.
    Gemini,
    /// AWS Bedrock — SigV4-signed `POST /model/<id>/invoke`. Reads
    /// `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`
    /// from env, region from `AWS_REGION` (default us-east-1).
    Bedrock,
    /// Google Vertex AI — Gemini wire format, auth via `gcloud auth
    /// print-access-token`. Reads `GOOGLE_CLOUD_PROJECT` and
    /// `GOOGLE_CLOUD_REGION` (default us-central1) from env.
    Vertex,
    /// OpenAI Codex — shells out to the `codex` CLI so the LLM call is
    /// billed against the user's ChatGPT subscription quota (via the
    /// `codex login` OAuth token) instead of a per-token API key. No
    /// HTTP base URL or API-key env var; auth lives in `~/.codex/`.
    Codex,
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
            "openrouter" => (
                "https://openrouter.ai/api/v1",
                "OPENROUTER_API_KEY",
                Wire::OpenAi,
            ),
            "nous" => (
                "https://inference-api.nousresearch.com/v1",
                "NOUS_API_KEY",
                Wire::OpenAi,
            ),
            "novita" => (
                "https://api.novita.ai/v3/openai",
                "NOVITA_API_KEY",
                Wire::OpenAi,
            ),
            "moonshot" => (
                "https://api.moonshot.ai/v1",
                "MOONSHOT_API_KEY",
                Wire::OpenAi,
            ),
            "minimax" => (
                "https://api.minimaxi.chat/v1",
                "MINIMAX_API_KEY",
                Wire::OpenAi,
            ),
            "zai" | "glm" => ("https://api.z.ai/api/paas/v4", "ZAI_API_KEY", Wire::OpenAi),
            "groq" => (
                "https://api.groq.com/openai/v1",
                "GROQ_API_KEY",
                Wire::OpenAi,
            ),
            "deepseek" => (
                "https://api.deepseek.com/v1",
                "DEEPSEEK_API_KEY",
                Wire::OpenAi,
            ),
            "anthropic" => (
                "https://api.anthropic.com/v1",
                "ANTHROPIC_API_KEY",
                Wire::Anthropic,
            ),
            "gemini" => (
                "https://generativelanguage.googleapis.com/v1beta",
                "GEMINI_API_KEY",
                Wire::Gemini,
            ),
            // Bedrock/Vertex don't use base_url or api_key_env — they each
            // have their own credential mechanism (SigV4 / gcloud OAuth).
            // The placeholders below are kept for `merlion doctor`'s probe.
            "bedrock" => (
                "https://bedrock-runtime.us-east-1.amazonaws.com",
                "AWS_ACCESS_KEY_ID",
                Wire::Bedrock,
            ),
            "vertex" => (
                "https://us-central1-aiplatform.googleapis.com",
                "GOOGLE_CLOUD_PROJECT",
                Wire::Vertex,
            ),
            // Codex shells out to the local `codex` CLI; there's no HTTP
            // base URL or env-var-driven key. The placeholders satisfy
            // ResolvedProvider's shape but aren't read at runtime.
            "codex" => (
                "(codex CLI — local subprocess)",
                "(codex login / ~/.codex/auth.json)",
                Wire::Codex,
            ),
            other => {
                anyhow::bail!(
                    "unknown provider `{other}`. Set `model.base_url` and `model.api_key_env` explicitly, \
                     or use one of: openai, openrouter, nous, novita, moonshot, minimax, zai, groq, deepseek, anthropic, gemini, bedrock, vertex, codex."
                );
            }
        };
        Ok(ResolvedProvider {
            model: model.to_string(),
            base_url: self
                .model
                .base_url
                .clone()
                .unwrap_or_else(|| default_base.to_string()),
            api_key_env: self
                .model
                .api_key_env
                .clone()
                .unwrap_or_else(|| default_env.to_string()),
            wire,
        })
    }
}

pub fn merlion_home() -> PathBuf {
    if let Ok(p) = std::env::var("MERLION_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .map(|h| h.join(".merlion"))
        .unwrap_or_else(|| PathBuf::from(".merlion"))
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

/// Ordered list of `"provider:model"` ids the runtime should fall through to
/// when the primary LLM returns a retriable error (429 / 5xx). Stored at
/// `~/.merlion/fallback.yaml`. A missing file is treated as an empty chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FallbackChain {
    #[serde(default)]
    pub chain: Vec<String>,
}

impl FallbackChain {
    pub fn default_path() -> PathBuf {
        merlion_home().join("fallback.yaml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::default_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: FallbackChain =
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<PathBuf> {
        let home = ensure_home()?;
        let path = home.join("fallback.yaml");
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

/// Module-private global lock used by every `#[cfg(test)]` block in this
/// file that mutates `MERLION_HOME`. Both `fallback_tests` and `auth_tests`
/// call into it so concurrent test runners can't race on the env var.
#[cfg(test)]
pub(crate) fn home_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    /// Point `merlion_home()` at a fresh temp dir for the duration of one test.
    /// `MERLION_HOME` is process-global, so we serialize via [`home_test_lock`].
    struct HomeGuard {
        prev: Option<String>,
        _tmp: std::path::PathBuf,
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                std::env::set_var("MERLION_HOME", prev);
            } else {
                std::env::remove_var("MERLION_HOME");
            }
            let _ = std::fs::remove_dir_all(&self._tmp);
        }
    }
    fn redirect_home() -> HomeGuard {
        let prev = std::env::var("MERLION_HOME").ok();
        let tmp = std::env::temp_dir().join(format!(
            "merlion-fallback-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create tmp home");
        std::env::set_var("MERLION_HOME", &tmp);
        HomeGuard { prev, _tmp: tmp }
    }

    #[test]
    fn fallback_load_returns_empty_when_file_missing() {
        let _guard = home_test_lock();
        let _home = redirect_home();
        let chain = FallbackChain::load().expect("load missing");
        assert!(chain.chain.is_empty());
    }

    #[test]
    fn fallback_save_then_load_roundtrip() {
        let _guard = home_test_lock();
        let _home = redirect_home();
        let chain = FallbackChain {
            chain: vec![
                "openrouter:anthropic/claude-sonnet-4".into(),
                "anthropic:claude-opus-4-7".into(),
            ],
        };
        let path = chain.save().expect("save");
        assert!(path.exists());
        assert_eq!(path, FallbackChain::default_path());

        let loaded = FallbackChain::load().expect("load");
        assert_eq!(loaded.chain, chain.chain);
    }

    #[test]
    fn fallback_default_path_uses_merlion_home() {
        let _guard = home_test_lock();
        let _home = redirect_home();
        let path = FallbackChain::default_path();
        assert_eq!(path, merlion_home().join("fallback.yaml"));
    }
}

/// State of a single pooled credential. `Ok` is the default; `Exhausted` is
/// flipped on when the upstream returns 429 and `exhausted_at` is stamped;
/// `Disabled` is set manually by the user (e.g. a key they want to keep on
/// disk but not use right now).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialState {
    #[default]
    Ok,
    Exhausted,
    Disabled,
}

/// One API key plus its bookkeeping. `token` is the raw secret; the rest of
/// the codebase MUST treat this struct as sensitive — never log `token`
/// directly, use [`redact_token`] when surfacing it to humans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PooledCredential {
    pub label: String,
    pub token: String,
    #[serde(default)]
    pub state: CredentialState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted_at: Option<DateTime<Utc>>,
}

impl PooledCredential {
    pub fn new(label: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            token: token.into(),
            state: CredentialState::Ok,
            exhausted_at: None,
        }
    }
}

/// Per-provider pool of API keys, persisted at `~/.merlion/auth.yaml`.
///
/// The pool is the source of truth for *which keys exist*. The actual
/// rotation policy (which key to grab, when to flip a key to `Exhausted`
/// after a 429) lives in `merlion-llm`. This crate only stores and looks up.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthPool {
    #[serde(default)]
    pub pools: BTreeMap<String, Vec<PooledCredential>>,
}

impl AuthPool {
    pub fn default_path() -> PathBuf {
        merlion_home().join("auth.yaml")
    }

    /// Load `~/.merlion/auth.yaml`. A missing file yields an empty pool —
    /// fresh installs and CI runs without persisted auth should both work.
    pub fn load() -> Result<Self> {
        let path = Self::default_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: AuthPool =
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<PathBuf> {
        let home = ensure_home()?;
        let path = home.join("auth.yaml");
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }

    /// Append a credential to the provider's pool. If a credential with the
    /// same label already exists, it is replaced — labels are unique within a
    /// pool so `add` doubles as "update".
    pub fn add(&mut self, provider: &str, cred: PooledCredential) {
        let entry = self.pools.entry(provider.to_string()).or_default();
        if let Some(existing) = entry.iter_mut().find(|c| c.label == cred.label) {
            *existing = cred;
        } else {
            entry.push(cred);
        }
    }

    /// Remove a single credential by label. Returns the removed credential if
    /// it was present. Empties the provider key entirely once its pool is
    /// empty so `auth.yaml` stays tidy.
    pub fn remove(&mut self, provider: &str, label: &str) -> Option<PooledCredential> {
        let pool = self.pools.get_mut(provider)?;
        let idx = pool.iter().position(|c| c.label == label)?;
        let removed = pool.remove(idx);
        if pool.is_empty() {
            self.pools.remove(provider);
        }
        Some(removed)
    }

    /// Mark every credential in `provider`'s pool back to `Ok` and clear any
    /// `exhausted_at` timestamp. No-op if the provider has no pool.
    pub fn reset(&mut self, provider: &str) {
        if let Some(pool) = self.pools.get_mut(provider) {
            for cred in pool.iter_mut() {
                cred.state = CredentialState::Ok;
                cred.exhausted_at = None;
            }
        }
    }

    /// First credential in `provider`'s pool whose state is `Ok`. The pool is
    /// iterated in insertion order, so labelling matters — the first added
    /// key is the default until it gets exhausted.
    pub fn first_ok(&self, provider: &str) -> Option<&PooledCredential> {
        self.pools
            .get(provider)?
            .iter()
            .find(|c| c.state == CredentialState::Ok)
    }

    /// Flip a labelled credential to `Exhausted` and stamp `exhausted_at`.
    /// No-op when the label isn't found, so callers can fire-and-forget from
    /// a 429 handler without first checking that the key still exists.
    pub fn mark_exhausted(&mut self, provider: &str, label: &str) {
        if let Some(pool) = self.pools.get_mut(provider) {
            if let Some(cred) = pool.iter_mut().find(|c| c.label == label) {
                cred.state = CredentialState::Exhausted;
                cred.exhausted_at = Some(Utc::now());
            }
        }
    }
}

/// Render a token as `…last4` for display. Returns `"<empty>"` for an empty
/// string and `"…<token>"` for tokens shorter than 4 chars so we never imply
/// a short token is being redacted when it isn't.
pub fn redact_token(token: &str) -> String {
    if token.is_empty() {
        return "<empty>".to_string();
    }
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 4 {
        return format!("…{token}");
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    struct HomeGuard {
        prev: Option<String>,
        _tmp: std::path::PathBuf,
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                std::env::set_var("MERLION_HOME", prev);
            } else {
                std::env::remove_var("MERLION_HOME");
            }
            let _ = std::fs::remove_dir_all(&self._tmp);
        }
    }
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        super::home_test_lock()
    }
    fn redirect_home() -> HomeGuard {
        let prev = std::env::var("MERLION_HOME").ok();
        let tmp = std::env::temp_dir().join(format!(
            "merlion-auth-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).expect("create tmp home");
        std::env::set_var("MERLION_HOME", &tmp);
        HomeGuard { prev, _tmp: tmp }
    }

    #[test]
    fn auth_load_returns_empty_when_file_missing() {
        let _guard = lock();
        let _home = redirect_home();
        let pool = AuthPool::load().expect("load missing");
        assert!(pool.pools.is_empty());
    }

    #[test]
    fn auth_roundtrip_preserves_all_three_states() {
        let _guard = lock();
        let _home = redirect_home();

        let now = Utc::now();
        let mut pool = AuthPool::default();
        pool.add(
            "openai",
            PooledCredential {
                label: "personal".into(),
                token: "sk-aaa".into(),
                state: CredentialState::Ok,
                exhausted_at: None,
            },
        );
        pool.add(
            "openai",
            PooledCredential {
                label: "work".into(),
                token: "sk-bbb".into(),
                state: CredentialState::Exhausted,
                exhausted_at: Some(now),
            },
        );
        pool.add(
            "openai",
            PooledCredential {
                label: "spare".into(),
                token: "sk-ccc".into(),
                state: CredentialState::Disabled,
                exhausted_at: None,
            },
        );

        let path = pool.save().expect("save");
        assert!(path.exists());
        assert_eq!(path, AuthPool::default_path());

        let loaded = AuthPool::load().expect("load");
        let openai = loaded.pools.get("openai").expect("openai pool");
        assert_eq!(openai.len(), 3);
        assert_eq!(openai[0].state, CredentialState::Ok);
        assert_eq!(openai[1].state, CredentialState::Exhausted);
        assert!(openai[1].exhausted_at.is_some());
        assert_eq!(openai[2].state, CredentialState::Disabled);
    }

    #[test]
    fn auth_add_replaces_existing_label() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("default", "sk-old"));
        pool.add("openai", PooledCredential::new("default", "sk-new"));
        let openai = pool.pools.get("openai").unwrap();
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0].token, "sk-new");
    }

    #[test]
    fn auth_remove_returns_credential_and_clears_empty_provider() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("default", "sk-1"));
        let removed = pool.remove("openai", "default");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().token, "sk-1");
        assert!(!pool.pools.contains_key("openai"));
        assert!(pool.remove("openai", "default").is_none());
    }

    #[test]
    fn auth_reset_clears_state_and_timestamp() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("a", "sk-1"));
        pool.add("openai", PooledCredential::new("b", "sk-2"));
        pool.mark_exhausted("openai", "a");
        pool.mark_exhausted("openai", "b");
        pool.reset("openai");
        let openai = pool.pools.get("openai").unwrap();
        for c in openai {
            assert_eq!(c.state, CredentialState::Ok);
            assert!(c.exhausted_at.is_none());
        }
    }

    #[test]
    fn auth_first_ok_skips_exhausted_and_disabled() {
        let mut pool = AuthPool::default();
        pool.add(
            "openai",
            PooledCredential {
                label: "a".into(),
                token: "sk-1".into(),
                state: CredentialState::Exhausted,
                exhausted_at: Some(Utc::now()),
            },
        );
        pool.add(
            "openai",
            PooledCredential {
                label: "b".into(),
                token: "sk-2".into(),
                state: CredentialState::Disabled,
                exhausted_at: None,
            },
        );
        pool.add("openai", PooledCredential::new("c", "sk-3"));

        let first = pool.first_ok("openai").expect("some Ok credential");
        assert_eq!(first.label, "c");
        assert!(pool.first_ok("missing").is_none());
    }

    #[test]
    fn auth_mark_exhausted_sets_state_and_timestamp() {
        let mut pool = AuthPool::default();
        pool.add("openai", PooledCredential::new("default", "sk-1"));
        pool.mark_exhausted("openai", "default");
        let cred = &pool.pools["openai"][0];
        assert_eq!(cred.state, CredentialState::Exhausted);
        assert!(cred.exhausted_at.is_some());
        // No-op on missing label / provider — must not panic.
        pool.mark_exhausted("openai", "nope");
        pool.mark_exhausted("anthropic", "default");
    }

    #[test]
    fn redact_token_keeps_only_last_four() {
        assert_eq!(redact_token("sk-1234567890"), "…7890");
        assert_eq!(redact_token("abcd"), "…abcd");
        assert_eq!(redact_token("ab"), "…ab");
        assert_eq!(redact_token(""), "<empty>");
    }
}
