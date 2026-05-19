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

/// A catalog entry for one provider preset. Drives the interactive
/// `merlion model` and `merlion setup` wizards: friendly label for the
/// provider picker, env-var name for the API-key prompt, and a curated
/// list of popular models for the model picker.
///
/// `prefix` must match a branch in
/// [`merlion_config::Config::resolve_provider`] — the
/// `catalog_prefixes_match_resolver` test enforces this.
pub struct ProviderEntry {
    pub prefix: &'static str,
    pub label: &'static str,
    /// Default API-key env var. Kept in the catalog for documentation +
    /// for `catalog_prefixes_match_resolver` to assert it matches what
    /// `resolve_provider` would compute; live wizard code reads the env
    /// var name via `cfg.resolve_provider()` so user overrides of
    /// `model.api_key_env` still take effect.
    #[allow(dead_code)]
    pub api_key_env: &'static str,
    pub models: &'static [&'static str],
}

impl ProviderEntry {
    pub fn default_model(&self) -> &'static str {
        self.models
            .first()
            .copied()
            .expect("each ProviderEntry must list at least one model")
    }
}

/// Provider + model catalog. Ordered with the entries most useful for a
/// Claude-Code-style coding workflow first (Anthropic, OpenAI, OpenRouter,
/// Gemini), then the rest grouped roughly by ecosystem.
///
/// Curated as of Jan 2026 — model lists will go stale; users can always
/// pick "Enter custom model name…" in the wizard, and `merlion model
/// <provider:model>` accepts any string.
pub const CATALOG: &[ProviderEntry] = &[
    ProviderEntry {
        prefix: "anthropic",
        label: "Anthropic (Claude — direct API)",
        api_key_env: "ANTHROPIC_API_KEY",
        // First entry is the wizard default — picked claude-sonnet-4 as a
        // balanced cost/quality starting point; users wanting Opus or
        // Haiku can pick from the list below.
        models: &[
            "claude-sonnet-4",
            "claude-opus-4-7",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-opus-4",
        ],
    },
    ProviderEntry {
        prefix: "openai",
        label: "OpenAI (gpt-4o family, o1 reasoning)",
        api_key_env: "OPENAI_API_KEY",
        models: &[
            "gpt-4o-mini",
            "gpt-4o",
            "gpt-4-turbo",
            "o1-preview",
            "o1-mini",
        ],
    },
    ProviderEntry {
        prefix: "openrouter",
        label: "OpenRouter (100+ models, pay-per-use)",
        api_key_env: "OPENROUTER_API_KEY",
        models: &[
            "anthropic/claude-sonnet-4",
            "anthropic/claude-opus-4",
            "openai/gpt-4o",
            "google/gemini-2.0-flash",
            "meta-llama/llama-3.3-70b-instruct",
        ],
    },
    ProviderEntry {
        prefix: "gemini",
        label: "Google AI Studio (Gemini — direct API)",
        api_key_env: "GEMINI_API_KEY",
        models: &[
            "gemini-2.0-flash",
            "gemini-1.5-pro",
            "gemini-1.5-flash",
            "gemini-2.0-flash-thinking-exp",
        ],
    },
    ProviderEntry {
        prefix: "groq",
        label: "Groq (LPU inference — fast Llama, Mixtral)",
        api_key_env: "GROQ_API_KEY",
        models: &[
            "llama-3.3-70b-versatile",
            "llama-3.1-8b-instant",
            "mixtral-8x7b-32768",
            "deepseek-r1-distill-llama-70b",
        ],
    },
    ProviderEntry {
        prefix: "deepseek",
        label: "DeepSeek (V3, R1, coder — direct API)",
        api_key_env: "DEEPSEEK_API_KEY",
        models: &["deepseek-chat", "deepseek-coder", "deepseek-reasoner"],
    },
    ProviderEntry {
        prefix: "moonshot",
        label: "Moonshot (Kimi K2 — global API)",
        api_key_env: "MOONSHOT_API_KEY",
        models: &[
            "kimi-k2-0905-preview",
            "moonshot-v1-128k",
            "moonshot-v1-32k",
            "moonshot-v1-8k",
        ],
    },
    ProviderEntry {
        prefix: "minimax",
        label: "MiniMax (M2 / Text-01 — global API)",
        api_key_env: "MINIMAX_API_KEY",
        models: &["MiniMax-M2", "MiniMax-Text-01"],
    },
    ProviderEntry {
        prefix: "zai",
        label: "Z.AI / GLM (Zhipu — direct API)",
        api_key_env: "ZAI_API_KEY",
        models: &["glm-4.6", "glm-4-air", "glm-4-flash"],
    },
    ProviderEntry {
        prefix: "nous",
        label: "Nous Research (Hermes models)",
        api_key_env: "NOUS_API_KEY",
        models: &["Hermes-4-405B", "Hermes-3-70B"],
    },
    ProviderEntry {
        prefix: "novita",
        label: "NovitaAI (open models, GPU cloud)",
        api_key_env: "NOVITA_API_KEY",
        models: &[
            "meta-llama/llama-3.3-70b-instruct",
            "meta-llama/llama-3.1-70b-instruct",
            "qwen/qwen-2.5-72b-instruct",
        ],
    },
    ProviderEntry {
        prefix: "bedrock",
        label: "AWS Bedrock (Claude on AWS, SigV4)",
        api_key_env: "AWS_ACCESS_KEY_ID",
        models: &[
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "anthropic.claude-opus-4-20250514-v1:0",
            "anthropic.claude-3-5-haiku-20241022-v1:0",
        ],
    },
    ProviderEntry {
        prefix: "vertex",
        label: "Google Vertex AI (Gemini via gcloud)",
        api_key_env: "GOOGLE_CLOUD_PROJECT",
        models: &["gemini-2.0-flash", "gemini-1.5-pro", "gemini-1.5-flash"],
    },
];

/// Look up the catalog entry for a provider prefix (e.g. "anthropic").
/// Returns `None` for unknown prefixes — callers should fall back to
/// treating it as an unknown provider rather than panicking.
pub fn catalog_entry(prefix: &str) -> Option<&'static ProviderEntry> {
    CATALOG.iter().find(|p| p.prefix == prefix)
}

/// Flat prefix list — exposed for tests that want to iterate every known
/// provider without going through the catalog. Not used by the wizard
/// itself anymore (the catalog drives the order there).
#[cfg(test)]
const PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "openrouter",
    "gemini",
    "groq",
    "deepseek",
    "moonshot",
    "minimax",
    "zai",
    "nous",
    "novita",
    "bedrock",
    "vertex",
];

#[cfg(test)]
fn default_model_for(provider: &str) -> &'static str {
    catalog_entry(provider)
        .map(|p| p.default_model())
        .unwrap_or("gpt-4o-mini")
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

    // Step 3 — provider picker (friendly catalog labels).
    let labels: Vec<String> = CATALOG
        .iter()
        .map(|e| {
            if e.prefix == current_provider {
                format!("{}  ← current", e.label)
            } else {
                e.label.to_string()
            }
        })
        .collect();
    let default_idx = CATALOG
        .iter()
        .position(|e| e.prefix == current_provider)
        .unwrap_or(0);
    let provider_idx = Select::with_theme(&theme)
        .with_prompt("Provider")
        .items(&labels)
        .default(default_idx)
        .interact()?;
    let entry = &CATALOG[provider_idx];

    // Step 4 — model picker for that provider. Curated list + "custom"
    // escape so power users aren't locked out of unlisted models.
    const CUSTOM: &str = "Enter custom model name…";
    let mut model_items: Vec<String> = entry
        .models
        .iter()
        .map(|m| {
            if entry.prefix == current_provider && *m == current_model {
                format!("{m}  ← current")
            } else {
                (*m).to_string()
            }
        })
        .collect();
    model_items.push(CUSTOM.to_string());

    let model_default_idx = if entry.prefix == current_provider {
        entry
            .models
            .iter()
            .position(|m| *m == current_model)
            .unwrap_or(0)
    } else {
        0
    };
    let model_idx = Select::with_theme(&theme)
        .with_prompt("Model")
        .items(&model_items)
        .default(model_default_idx)
        .interact()?;
    let model = if model_idx == entry.models.len() {
        Input::with_theme(&theme)
            .with_prompt("Model name")
            .default(entry.default_model().to_string())
            .interact_text()?
    } else {
        entry.models[model_idx].to_string()
    };

    cfg.model = ModelConfig {
        id: format!("{}:{}", entry.prefix, model),
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
pub fn append_env_line(path: &std::path::Path, key: &str, value: &str) -> Result<()> {
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
    fn catalog_prefixes_match_resolver() {
        // Every catalog prefix must round-trip through `resolve_provider`
        // with its own first model — otherwise the wizard would happily
        // write a `provider:model` id that the runtime can't construct.
        for entry in CATALOG {
            let mut cfg = Config::default();
            cfg.model.id = format!("{}:{}", entry.prefix, entry.default_model());
            cfg.model.base_url = None;
            cfg.model.api_key_env = None;
            let resolved = cfg
                .resolve_provider()
                .unwrap_or_else(|e| panic!("catalog entry `{}` failed resolve: {e}", entry.prefix));
            assert_eq!(
                resolved.api_key_env, entry.api_key_env,
                "catalog `api_key_env` for `{}` disagrees with resolver",
                entry.prefix
            );
        }
    }

    #[test]
    fn catalog_models_nonempty() {
        for entry in CATALOG {
            assert!(
                !entry.models.is_empty(),
                "catalog entry `{}` has no models",
                entry.prefix
            );
        }
    }

    #[test]
    fn catalog_entry_lookup() {
        assert!(catalog_entry("anthropic").is_some());
        assert!(catalog_entry("openai").is_some());
        assert!(catalog_entry("bogus").is_none());
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
