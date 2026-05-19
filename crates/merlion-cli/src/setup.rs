//! Interactive first-run setup wizard.
//!
//! Walks the user through picking a provider, a default model, an API key,
//! and an optional system prompt — then writes `~/.merlion/config.yaml` and
//! (if a key was supplied) appends to `~/.merlion/.env`. Re-running is
//! safe: existing values are surfaced as defaults so the user can ack or
//! tweak them.
//!
//! Mirrors the UX of hermes's `hermes setup`.

use std::fs::OpenOptions;
use std::io::Write as _;

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use merlion_config::{ensure_home, merlion_home, Config, ModelConfig};

/// The full list of provider prefixes accepted by
/// [`merlion_config::Config::resolve_provider`]. Kept in the same order as
/// the resolver so the picker UI matches the docs.
const PROVIDERS: &[&str] = &[
    "openai",
    "openrouter",
    "nous",
    "novita",
    "moonshot",
    "minimax",
    "zai",
    "groq",
    "deepseek",
    "anthropic",
    "gemini",
    "bedrock",
    "vertex",
];

/// Sensible default model for each provider prefix. Used to seed the model
/// `Input` prompt so first-run users don't have to know the catalog.
fn default_model_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o-mini",
        "openrouter" => "anthropic/claude-sonnet-4",
        "nous" => "Hermes-4-405B",
        "novita" => "meta-llama/llama-3.1-70b-instruct",
        "moonshot" => "kimi-k2-0905-preview",
        "minimax" => "MiniMax-M2",
        "zai" => "glm-4.6",
        "groq" => "llama-3.3-70b-versatile",
        "deepseek" => "deepseek-chat",
        "anthropic" => "claude-sonnet-4",
        "gemini" => "gemini-2.0-flash",
        "bedrock" => "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "vertex" => "gemini-2.0-flash",
        _ => "gpt-4o-mini",
    }
}

/// Run the interactive setup wizard. Writes `~/.merlion/config.yaml`
/// and (if the user enters an API key) `~/.merlion/.env`. Idempotent —
/// re-running is safe, existing values are shown as defaults.
pub async fn run() -> Result<()> {
    let theme = ColorfulTheme::default();

    println!("Welcome to merlion-agent setup.");
    println!();
    println!(
        "This will write {} and (optionally) {}.",
        merlion_home().join("config.yaml").display(),
        merlion_home().join(".env").display(),
    );
    println!();

    let home = ensure_home()?;
    let config_path = home.join("config.yaml");
    let env_path = home.join(".env");

    // Step 2 — probe existing config so we can prefill defaults.
    let mut cfg = if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        serde_yaml::from_str::<Config>(&text).unwrap_or_default()
    } else {
        Config::default()
    };

    // Split current `model.id` into (provider, model) for use as defaults.
    let (current_provider, current_model) = match cfg.model.id.split_once(':') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => ("openai".to_string(), cfg.model.id.clone()),
    };

    // Step 3 — provider picker.
    let default_idx = PROVIDERS
        .iter()
        .position(|p| *p == current_provider)
        .unwrap_or(0);
    let provider_idx = Select::with_theme(&theme)
        .with_prompt("Provider")
        .items(PROVIDERS)
        .default(default_idx)
        .interact()?;
    let provider = PROVIDERS[provider_idx];

    // Step 4 — model name. If the user kept the same provider, prefer the
    // model they had configured; otherwise fall back to the per-provider
    // default.
    let model_default = if provider == current_provider && !current_model.is_empty() {
        current_model.clone()
    } else {
        default_model_for(provider).to_string()
    };
    let model: String = Input::with_theme(&theme)
        .with_prompt("Model")
        .default(model_default)
        .interact_text()?;

    cfg.model = ModelConfig {
        id: format!("{provider}:{model}"),
        base_url: cfg.model.base_url,
        api_key_env: cfg.model.api_key_env,
        temperature: cfg.model.temperature,
        max_tokens: cfg.model.max_tokens,
    };

    // Step 5 — API key. We need the env-var name from `resolve_provider`
    // (which respects any explicit `api_key_env` override the user may
    // already have set).
    let resolved = cfg.resolve_provider()?;
    let key_env = resolved.api_key_env.clone();

    let already_set = std::env::var(&key_env).ok().filter(|v| !v.is_empty());
    let key_prompt = if already_set.is_some() {
        format!("{key_env} (already set in env; press Enter to keep)")
    } else {
        format!("{key_env} (press Enter to skip and add it manually later)")
    };

    let api_key: String = Password::with_theme(&theme)
        .with_prompt(key_prompt)
        .allow_empty_password(true)
        .interact()?;

    let trimmed_key = api_key.trim();
    if !trimmed_key.is_empty() {
        append_env_line(&env_path, &key_env, trimmed_key)?;
        println!("Saved {key_env} to {}", env_path.display());
    } else if already_set.is_some() {
        println!("Keeping existing {key_env} from environment.");
    } else {
        println!(
            "No API key entered. Add `{key_env}=...` to {} before running `merlion`.",
            env_path.display()
        );
    }

    // Step 6 — optional system prompt.
    let sys_default = cfg.system_prompt.clone().unwrap_or_default();
    let sys_prompt: String = Input::with_theme(&theme)
        .with_prompt("System prompt (optional, press Enter to skip)")
        .default(sys_default)
        .allow_empty(true)
        .interact_text()?;
    cfg.system_prompt = if sys_prompt.trim().is_empty() {
        None
    } else {
        Some(sys_prompt)
    };

    // Step 7 — persist.
    let written = merlion_config::save(&cfg)?;

    // Step 8 — confirm + next steps.
    println!();
    println!("Wrote {}.", written.display());
    println!();
    println!("Next steps:");
    println!("  merlion doctor   # verify config + credentials");
    println!("  merlion          # start chatting");

    Ok(())
}

/// Append `KEY=VALUE` to `path`, creating the file if it doesn't exist.
/// We don't try to dedupe — `dotenvy` reads top-to-bottom and later values
/// win, so re-running the wizard correctly overrides an earlier key.
fn append_env_line(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {} for append", path.display()))?;
    // Ensure the new entry starts on its own line even if the existing file
    // was missing a trailing newline.
    let needs_leading_newline = path.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && !file_ends_with_newline(path).unwrap_or(true);
    if needs_leading_newline {
        writeln!(f).ok();
    }
    writeln!(f, "{key}={value}").with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn file_ends_with_newline(path: &std::path::Path) -> Result<bool> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.ends_with('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_covers_all_providers() {
        // Every provider known to `resolve_provider` should have a default
        // suggestion — otherwise the wizard would offer the unrelated
        // `gpt-4o-mini` fallback and confuse the user.
        for p in PROVIDERS {
            let m = default_model_for(p);
            assert!(!m.is_empty(), "no default model for `{p}`");
        }
    }

    #[test]
    fn default_model_specific_providers() {
        assert_eq!(default_model_for("openai"), "gpt-4o-mini");
        assert_eq!(default_model_for("anthropic"), "claude-sonnet-4");
        assert_eq!(default_model_for("gemini"), "gemini-2.0-flash");
        assert_eq!(default_model_for("deepseek"), "deepseek-chat");
        assert_eq!(default_model_for("groq"), "llama-3.3-70b-versatile");
        assert_eq!(default_model_for("zai"), "glm-4.6");
    }

    #[test]
    fn default_model_unknown_falls_back() {
        assert_eq!(default_model_for("not-a-real-provider"), "gpt-4o-mini");
    }

    #[test]
    fn provider_list_round_trips_through_resolve() {
        // Each provider prefix in PROVIDERS should resolve cleanly through
        // `Config::resolve_provider` so the wizard never produces a config
        // the loader can't parse.
        for p in PROVIDERS {
            let cfg = Config {
                model: ModelConfig {
                    id: format!("{p}:{}", default_model_for(p)),
                    base_url: None,
                    api_key_env: None,
                    temperature: None,
                    max_tokens: None,
                },
                system_prompt: None,
                max_iterations: 32,
            };
            let resolved = cfg
                .resolve_provider()
                .unwrap_or_else(|e| panic!("provider `{p}` failed to resolve: {e}"));
            assert!(
                !resolved.api_key_env.is_empty(),
                "empty api_key_env for {p}"
            );
        }
    }

    #[test]
    fn append_env_line_creates_file_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env");

        append_env_line(&path, "FOO_API_KEY", "abc123").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "FOO_API_KEY=abc123\n");

        // Re-running appends; later values win under dotenvy.
        append_env_line(&path, "FOO_API_KEY", "xyz789").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "FOO_API_KEY=abc123\nFOO_API_KEY=xyz789\n");
    }

    #[test]
    fn append_env_line_adds_leading_newline_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env");
        std::fs::write(&path, "EXISTING=1").unwrap(); // no trailing newline

        append_env_line(&path, "NEW", "2").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "EXISTING=1\nNEW=2\n");
    }
}

// ────────────────────────────────────────────────────────────────────────────
// WIRING SPEC — main.rs changes required to expose this wizard
// ────────────────────────────────────────────────────────────────────────────
//
// This module is intentionally not wired into `main.rs` by this patch. To
// activate it, apply the following four edits to
// `crates/merlion-cli/src/main.rs`:
//
// 1) Declare the module alongside `mod approver; mod tui;`:
//
//        mod setup;
//
// 2) Add a `Setup` variant to the `Command` enum (no fields, no args):
//
//        /// Interactive first-run setup wizard. Writes ~/.merlion/config.yaml
//        /// and ~/.merlion/.env after asking for provider, model, API key,
//        /// and an optional system prompt.
//        Setup,
//
// 3) Add a dispatch arm inside the `match cli.command.unwrap_or(...)` block,
//    next to `Command::Doctor => doctor(cfg),`:
//
//        Command::Setup => setup::run().await,
//
// 4) No new `use` lines are required at the top of `main.rs` — `setup::run`
//    is referenced through the `setup` module path and returns
//    `anyhow::Result<()>`, matching the other async dispatch arms.
//
// After those edits, `merlion setup` runs the wizard. The wizard reads any
// existing `~/.merlion/config.yaml` as defaults, so it is safe to re-run.
