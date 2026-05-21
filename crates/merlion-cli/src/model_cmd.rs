//! `merlion model` — print, set, or interactively pick the active model.
//!
//! Behavior:
//! - `merlion model` (no arg) → interactive wizard backed by
//!   [`crate::setup::CATALOG`]: friendly provider menu → curated model
//!   menu (with "Enter custom" + "Keep current" escapes) → optional
//!   API-key prompt. Mirrors `hermes model`'s UX sans the Nous-portal
//!   OAuth flow, which is Nous-internal infrastructure.
//! - `merlion model <provider:model>` → set the value directly. If the
//!   provider's API-key env var isn't currently set, prompt for it inline
//!   so you don't have to chase down a second command.
//!
//! Use `merlion config show` or `merlion doctor` to view the current
//! setting without launching the wizard.
//!
//! Writes `~/.merlion/config.yaml` and (if a key was entered) appends to
//! `~/.merlion/.env`.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use merlion_config::{ensure_home, Config, ModelConfig};

use crate::setup::{append_env_line, catalog_entry, CATALOG};

pub fn run(cfg: Config, id: Option<String>) -> Result<()> {
    match id {
        Some(new_id) => set_shortcut(cfg, new_id),
        None => wizard(cfg),
    }
}

/// `merlion model <provider:model>` path. Writes config, then prompts for
/// the API key only if its env var isn't already populated.
fn set_shortcut(mut cfg: Config, new_id: String) -> Result<()> {
    cfg.model = ModelConfig {
        id: new_id,
        // Clear any override that was specific to the old model so the next
        // `resolve_provider()` call uses the new prefix's preset.
        base_url: None,
        api_key_env: None,
        temperature: cfg.model.temperature,
        max_tokens: cfg.model.max_tokens,
    };
    let path = merlion_config::save(&cfg)?;
    println!("Set model = {} ({})", cfg.model.id, path.display());

    // Codex auth lives in ~/.codex/auth.json, not an env var. Skip the
    // API-key prompt entirely and just report status.
    if cfg.model.id.starts_with("codex:") {
        report_codex_auth_status();
        return Ok(());
    }

    let resolved = cfg.resolve_provider()?;
    let key_env = resolved.api_key_env;
    if std::env::var(&key_env)
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        let home = ensure_home()?;
        let env_path = home.join(".env");
        prompt_and_save_key(&env_path, &key_env)?;
    }
    Ok(())
}

/// `merlion model` (no arg) path — catalog-driven interactive picker.
fn wizard(mut cfg: Config) -> Result<()> {
    // dialoguer's Select needs a real TTY for arrow-key input. When we're
    // piped (e.g. from Claude Code's bash tool, a script, or `merlion model
    // | tee`), fall back to printing the current setting plus a hint —
    // erroring out with "not a terminal" was much worse UX.
    if !std::io::stdin().is_terminal() {
        println!("{}", cfg.model.id);
        println!();
        println!("(stdin is not a terminal — interactive picker disabled.)");
        println!("To switch model from a non-TTY context, pass it as an argument:");
        println!("  merlion model anthropic:claude-sonnet-4");
        println!("  merlion model openrouter:anthropic/claude-sonnet-4");
        println!("Or run `merlion model` from a real terminal for the picker.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();
    let home = ensure_home()?;
    let env_path = home.join(".env");

    println!("Current model: {}", cfg.model.id);
    println!();

    let (current_provider, current_model) = match cfg.model.id.split_once(':') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (String::new(), cfg.model.id.clone()),
    };

    // ── Step 1 — provider picker (friendly labels) ──────────────────────
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
        .interact()
        .context("provider picker")?;
    let entry = &CATALOG[provider_idx];

    // ── Step 2 — model picker for that provider ─────────────────────────
    let model = pick_model(&theme, entry, &current_model, &current_provider)?;
    let Some(model) = model else {
        println!("No change.");
        return Ok(());
    };

    cfg.model = ModelConfig {
        id: format!("{}:{}", entry.prefix, model),
        base_url: None,
        api_key_env: None,
        temperature: cfg.model.temperature,
        max_tokens: cfg.model.max_tokens,
    };

    let path = merlion_config::save(&cfg)?;
    println!();
    println!("Set model = {} ({})", cfg.model.id, path.display());

    // ── Step 3 — credentials (codex uses ~/.codex/auth.json, not env) ───
    if entry.prefix == "codex" {
        report_codex_auth_status();
        return Ok(());
    }

    let resolved = cfg.resolve_provider()?;
    let key_env = resolved.api_key_env;
    if std::env::var(&key_env)
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        prompt_and_save_key(&env_path, &key_env)?;
    } else {
        println!("Using existing {key_env} from environment.");
    }

    Ok(())
}

/// Build the per-provider model menu. Returns `Some(model_name)` when the
/// user picked or entered a name, `None` when they chose "Keep current"
/// without changing anything.
fn pick_model(
    theme: &ColorfulTheme,
    entry: &crate::setup::ProviderEntry,
    current_model: &str,
    current_provider: &str,
) -> Result<Option<String>> {
    // Build the menu: catalog models, with the current selection marked
    // when the user is staying on the same provider; then escape options.
    const CUSTOM: &str = "Enter custom model name…";
    const KEEP: &str = "Keep current (no change)";

    let mut items: Vec<String> = entry
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
    items.push(CUSTOM.to_string());
    items.push(KEEP.to_string());

    let default_idx = if entry.prefix == current_provider {
        entry
            .models
            .iter()
            .position(|m| *m == current_model)
            .unwrap_or(0)
    } else {
        0
    };

    let idx = Select::with_theme(theme)
        .with_prompt("Model")
        .items(&items)
        .default(default_idx)
        .interact()
        .context("model picker")?;

    // Custom slot is at len-2, Keep at len-1.
    let custom_idx = entry.models.len();
    let keep_idx = entry.models.len() + 1;
    if idx == keep_idx {
        return Ok(None);
    }
    if idx == custom_idx {
        let custom: String = Input::with_theme(theme)
            .with_prompt("Model name")
            .default(entry.default_model().to_string())
            .interact_text()
            .context("custom model input")?;
        return Ok(Some(custom));
    }
    Ok(Some(entry.models[idx].to_string()))
}

/// Prompt for an API key (hidden input) and append it to `.env` if entered.
/// Empty input is allowed and treated as "I'll add it later."
fn prompt_and_save_key(env_path: &std::path::Path, key_env: &str) -> Result<()> {
    let theme = ColorfulTheme::default();
    let prompt = format!("{key_env} (press Enter to skip and add it manually later)");
    let key: String = Password::with_theme(&theme)
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()
        .context("API key input")?;
    let trimmed = key.trim();
    if trimmed.is_empty() {
        println!(
            "No API key entered. Add `{key_env}=...` to {} before running `merlion`.",
            env_path.display()
        );
    } else {
        append_env_line(env_path, key_env, trimmed)?;
        println!("Saved {key_env} to {}", env_path.display());
    }
    Ok(())
}

/// Resolve a `provider:model` id into a catalog entry if possible. Used by
/// `merlion doctor` and friends to print a friendly label for the current
/// selection.
#[allow(dead_code)]
/// For the `codex` provider, auth is held in `~/.codex/auth.json` (after
/// `codex login`), not in an env var. Skip the API-key prompt and just
/// report whether auth is present, with a clear hint when it isn't.
fn report_codex_auth_status() {
    let auth = dirs::home_dir().map(|h| h.join(".codex/auth.json"));
    let exists = auth.as_ref().map(|p| p.exists()).unwrap_or(false);
    if exists {
        println!("Codex auth detected ({}).", auth.unwrap().display());
    } else {
        println!("Codex auth not found. Run `codex login` to authenticate;");
        println!("the token will be saved to ~/.codex/auth.json.");
    }
}

/// Resolve a `provider:model` id into a catalog entry if possible. Used by
/// `merlion doctor` and friends to print a friendly label for the current
/// selection.
#[allow(dead_code)]
pub fn label_for_id(id: &str) -> Option<&'static str> {
    let (prefix, _) = id.split_once(':')?;
    catalog_entry(prefix).map(|e| e.label)
}
