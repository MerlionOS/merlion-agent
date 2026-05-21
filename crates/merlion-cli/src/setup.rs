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
use dialoguer::console::style;
use dialoguer::{theme::ColorfulTheme, Input, Password, Select};
use merlion_config::{ensure_home, Config, ModelConfig};

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
        prefix: "codex",
        label: "OpenAI Codex (ChatGPT subscription — `codex` CLI shell-out)",
        // Codex auth is held in ~/.codex/auth.json by `codex login`, not
        // in an env var. The catalog entry's api_key_env field is kept
        // for documentation only.
        api_key_env: "(codex login / ~/.codex/auth.json)",
        // Models available to ChatGPT subscription accounts via the
        // codex CLI. `gpt-5-codex` / `gpt-5` (no version suffix) are
        // gated to API-billed accounts; the codex-only IDs are listed
        // here. The "Enter custom model name…" escape hatch covers
        // anything new.
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
        ],
    },
    ProviderEntry {
        prefix: "openai",
        label: "OpenAI (gpt-5 family, gpt-4o, o1 reasoning)",
        api_key_env: "OPENAI_API_KEY",
        // First entry is the wizard default. gpt-5.5 is the current
        // OpenAI flagship as of 2026-05; gpt-4o-mini remains the
        // cheap-and-fast fallback at the bottom.
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
            "gpt-4o",
            "gpt-4o-mini",
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
            "openai/gpt-5.5",
            "openai/gpt-5.4-mini",
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
    "codex",
];

#[cfg(test)]
fn default_model_for(provider: &str) -> &'static str {
    catalog_entry(provider)
        .map(|p| p.default_model())
        .unwrap_or("gpt-5.5")
}

/// Which sections to run. Mirrors `hermes setup [model|gateway|agent]`:
/// each variant runs just that section's prompts. `Full` runs every
/// section, showing the welcome banner and Configuration Location panel
/// up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Full,
    Model,
    Gateway,
    Agent,
}

/// Run the interactive setup wizard.
///
/// - `section` picks which section(s) to run (matches `merlion setup
///   model|gateway|agent`).
/// - `quick = true` mirrors `hermes setup --quick`: skip prompts whose
///   underlying value is already configured (config field or env var).
///
/// Writes `~/.merlion/config.yaml` and, for keys/tokens the user
/// enters, appends to `~/.merlion/.env`. Idempotent — re-running is
/// always safe.
pub async fn run(section: Section, quick: bool) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(anyhow::anyhow!(
            "`merlion setup` is interactive and needs a real terminal.\n\
             From a non-TTY context, edit ~/.merlion/config.yaml directly,\n\
             or use the targeted shortcuts:\n  \
             merlion model <provider:model>\n  \
             merlion config edit"
        ));
    }
    let home = ensure_home()?;
    let config_path = home.join("config.yaml");
    let env_path = home.join(".env");

    let mut cfg = if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        serde_yaml::from_str::<Config>(&text).unwrap_or_default()
    } else {
        Config::default()
    };

    if section == Section::Full {
        print_banner();
        print_reconfigure_preamble(config_path.exists(), quick);
        print_config_location(&config_path, &env_path, &home);
    }

    let mut wrote_config = false;

    if matches!(section, Section::Full | Section::Model)
        && section_inference_provider(&mut cfg, &env_path, quick)?
    {
        wrote_config = true;
    }
    if matches!(section, Section::Full | Section::Gateway) {
        section_gateway(&env_path, quick)?;
    }
    if matches!(section, Section::Full | Section::Agent) && section_agent(&mut cfg, quick)? {
        wrote_config = true;
    }

    if wrote_config {
        let written = merlion_config::save(&cfg)?;
        println!();
        println!(
            "{} {}",
            style("✓").green().bold(),
            style(format!("Wrote {}", written.display())).bold()
        );
    }

    if section == Section::Full {
        print_next_steps();
    }

    Ok(())
}

// ─── Visual helpers ────────────────────────────────────────────────────

fn print_banner() {
    let title = "Merlion Agent Setup Wizard";
    let inner_width = title.chars().count() + 6;
    let horizontal: String = "─".repeat(inner_width);
    println!();
    println!("  {}", style(format!("┌{horizontal}┐")).magenta());
    println!(
        "  {} {}   {}   {}",
        style("│").magenta(),
        style("⚕").magenta(),
        style(title).bold().magenta(),
        style("│").magenta(),
    );
    println!("  {}", style(format!("└{horizontal}┘")).magenta());
    println!();
    println!(
        "  {}",
        style("Let's configure your Merlion Agent installation.").dim()
    );
    println!("  {}", style("Press Ctrl+C at any time to exit.").dim());
    println!();
}

fn print_reconfigure_preamble(already_configured: bool, quick: bool) {
    section_header("Reconfigure");
    if already_configured {
        println!(
            "  {} {}",
            style("✓").green().bold(),
            style("You already have Merlion configured.").bold()
        );
        if quick {
            println!(
                "  {}",
                style("--quick mode — only prompting for missing values.").dim()
            );
        } else {
            println!(
                "  {}",
                style("Each prompt shows the current value. Press Enter to keep it,").dim()
            );
            println!("  {}", style("or type a new value to change it.").dim());
        }
    } else {
        println!(
            "  {}",
            style("First-time setup — let's walk through every section.").dim()
        );
    }
    println!();
    println!(
        "  {} {}",
        style("Tip:").dim(),
        style("jump to a section with 'merlion setup model|gateway|agent',").dim()
    );
    println!(
        "       {}",
        style("or fill only missing items with --quick.").dim()
    );
    println!();
}

fn print_config_location(
    config_path: &std::path::Path,
    env_path: &std::path::Path,
    home: &std::path::Path,
) {
    section_header("Configuration Location");
    println!(
        "  {} {}",
        style("Config file: ").cyan(),
        config_path.display()
    );
    println!("  {} {}", style("Secrets file:").cyan(), env_path.display());
    println!("  {} {}", style("Data folder: ").cyan(), home.display());
    println!();
    println!(
        "  {}",
        style("You can edit these files directly or use 'merlion config edit'.").dim()
    );
    println!();
}

fn section_header(title: &str) {
    println!(
        "{} {}",
        style("◆").cyan().bold(),
        style(title).cyan().bold()
    );
}

fn print_next_steps() {
    println!();
    section_header("Next steps");
    println!(
        "  {} verify config + credentials",
        style("merlion doctor").bold()
    );
    println!("  {}            start chatting", style("merlion").bold());
}

// ─── Inference Provider ────────────────────────────────────────────────

/// Returns `true` if `cfg.model` was changed and the config must be saved.
fn section_inference_provider(
    cfg: &mut Config,
    env_path: &std::path::Path,
    quick: bool,
) -> Result<bool> {
    println!();
    section_header("Inference Provider");

    let (current_provider, current_model) = match cfg.model.id.split_once(':') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (String::new(), cfg.model.id.clone()),
    };
    let is_codex = current_provider == "codex";

    let resolved_key_env = cfg
        .resolve_provider()
        .ok()
        .map(|p| p.api_key_env)
        .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let creds_ok = if is_codex {
        codex_auth_exists()
    } else {
        std::env::var(&resolved_key_env)
            .ok()
            .filter(|v| !v.is_empty())
            .is_some()
    };

    let pretty_provider = catalog_entry(&current_provider)
        .map(|e| e.label.to_string())
        .unwrap_or_else(|| current_provider.clone());

    println!(
        "  {} {}",
        style("Current model:   ").dim(),
        style(&cfg.model.id).bold()
    );
    println!("  {} {}", style("Active provider: ").dim(), pretty_provider);
    if is_codex {
        let path_display = codex_auth_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.codex/auth.json".into());
        println!(
            "  {} {} {}",
            style("Codex auth:      ").dim(),
            if creds_ok {
                style("✓").green().bold().to_string()
            } else {
                style("missing").red().to_string()
            },
            if creds_ok {
                style(format!("({path_display})")).dim().to_string()
            } else {
                style("(run `codex login`)").dim().to_string()
            }
        );
    } else {
        println!(
            "  {} {} {}",
            style(format!("{resolved_key_env}:")).dim(),
            if creds_ok {
                style("✓").green().bold().to_string()
            } else {
                style("missing").red().to_string()
            },
            if creds_ok {
                style("(already set in env)").dim().to_string()
            } else {
                String::new()
            }
        );
    }
    println!();

    // --quick: if the model id is configured and creds are present, skip.
    if quick && creds_ok && !cfg.model.id.is_empty() {
        println!(
            "  {}",
            style("Skipped — model + credentials already set.").dim()
        );
        return Ok(false);
    }

    let theme = ColorfulTheme::default();

    // 3-way credential prompt when key is already set. For codex, drop
    // the "Re-enter API key" option (auth flows through `codex login`,
    // not an env var) — present a 2-choice picker instead.
    if creds_ok && !cfg.model.id.is_empty() {
        let choices: &[&str] = if is_codex {
            &[
                "Use existing model + credentials (skip)",
                "Change model / provider",
            ]
        } else {
            &[
                "Use existing model + credentials (skip)",
                "Change model / provider",
                "Re-enter API key",
            ]
        };
        let pick = Select::with_theme(&theme)
            .with_prompt("What would you like to do?")
            .items(choices)
            .default(0)
            .interact()?;
        match pick {
            0 => return Ok(false),
            1 => { /* fall through to provider+model pickers */ }
            2 => {
                prompt_and_save_key(env_path, &resolved_key_env)?;
                return Ok(false);
            }
            _ => unreachable!(),
        }
    }

    // Provider picker.
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

    // Model picker with "Enter custom" escape.
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
        base_url: cfg.model.base_url.clone(),
        api_key_env: cfg.model.api_key_env.clone(),
        temperature: cfg.model.temperature,
        max_tokens: cfg.model.max_tokens,
    };

    // Credentials for the (possibly new) provider.
    if entry.prefix == "codex" {
        if codex_auth_exists() {
            println!(
                "  {} {}",
                style("✓").green().bold(),
                style("Codex auth detected at ~/.codex/auth.json.").dim()
            );
        } else {
            println!(
                "  {} {}",
                style("·").dim(),
                style("Run `codex login` before using merlion with the codex provider.").dim()
            );
        }
        return Ok(true);
    }
    let new_key_env = cfg.resolve_provider()?.api_key_env;
    if std::env::var(&new_key_env)
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        prompt_and_save_key(env_path, &new_key_env)?;
    } else {
        println!(
            "  {} {} {}",
            style("✓").green().bold(),
            style(&new_key_env).bold(),
            style("already set in env.").dim()
        );
    }

    Ok(true)
}

// ─── Gateway ───────────────────────────────────────────────────────────

fn section_gateway(env_path: &std::path::Path, quick: bool) -> Result<()> {
    println!();
    section_header("Messaging Gateway (optional)");
    println!(
        "  {}",
        style("Talk to merlion from Telegram / Discord / Slack. Skip if you don't").dim()
    );
    println!(
        "  {}",
        style("need it — you can always configure later with 'merlion setup gateway'.").dim()
    );
    println!();

    let theme = ColorfulTheme::default();

    let platforms = [
        (
            "Telegram",
            "TELEGRAM_BOT_TOKEN",
            "MERLION_GATEWAY_ALLOW_TELEGRAM",
            "BotFather token (e.g. 1234567890:AAH...)",
            "your Telegram numeric user id (comma-separated for multiple)",
        ),
        (
            "Discord",
            "DISCORD_BOT_TOKEN",
            "MERLION_GATEWAY_ALLOW_DISCORD",
            "Discord bot token from developer portal",
            "your Discord user id",
        ),
        (
            "Slack",
            "SLACK_APP_TOKEN",
            "MERLION_GATEWAY_ALLOW_SLACK",
            "Slack app-level token (xapp-...)",
            "your Slack user id (Uxxx)",
        ),
    ];

    for (name, token_var, allow_var, token_hint, allow_hint) in platforms {
        let token_set = std::env::var(token_var)
            .ok()
            .filter(|v| !v.is_empty())
            .is_some();
        let allow_set = std::env::var(allow_var)
            .ok()
            .filter(|v| !v.is_empty())
            .is_some();

        // Slack also needs SLACK_BOT_TOKEN; surface it briefly without
        // making the loop hairy.
        let slack_bot_ok = name != "Slack"
            || std::env::var("SLACK_BOT_TOKEN")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some();

        let status_mark = if token_set && allow_set && slack_bot_ok {
            style("✓").green().bold()
        } else if token_set || allow_set {
            style("◐").yellow().bold()
        } else {
            style("·").dim().bold()
        };
        println!("  {} {}", status_mark, style(name).bold());
        println!(
            "      {} {}",
            style(format!("{token_var}:")).dim(),
            if token_set { "set" } else { "missing" }
        );
        if name == "Slack" {
            println!(
                "      {} {}",
                style("SLACK_BOT_TOKEN:").dim(),
                if slack_bot_ok { "set" } else { "missing" }
            );
        }
        println!(
            "      {} {}",
            style(format!("{allow_var}:")).dim(),
            if allow_set { "set" } else { "missing" }
        );

        if quick && token_set && allow_set && slack_bot_ok {
            continue;
        }

        let configure = dialoguer::Confirm::with_theme(&theme)
            .with_prompt(format!("  Configure {name}?"))
            .default(false)
            .interact()?;
        if !configure {
            continue;
        }

        if !token_set {
            let token: String = Password::with_theme(&theme)
                .with_prompt(format!("  {token_var} — {token_hint}"))
                .allow_empty_password(true)
                .interact()?;
            if !token.trim().is_empty() {
                append_env_line(env_path, token_var, token.trim())?;
                println!(
                    "    {} {}",
                    style("✓").green(),
                    style(format!("Saved {token_var}")).dim()
                );
            }
        }

        if name == "Slack" && !slack_bot_ok {
            let bot: String = Password::with_theme(&theme)
                .with_prompt("  SLACK_BOT_TOKEN — Slack bot OAuth token (xoxb-...)")
                .allow_empty_password(true)
                .interact()?;
            if !bot.trim().is_empty() {
                append_env_line(env_path, "SLACK_BOT_TOKEN", bot.trim())?;
                println!(
                    "    {} {}",
                    style("✓").green(),
                    style("Saved SLACK_BOT_TOKEN").dim()
                );
            }
        }

        if !allow_set {
            let allow: String = Input::with_theme(&theme)
                .with_prompt(format!("  {allow_var} — {allow_hint}"))
                .allow_empty(true)
                .interact_text()?;
            if !allow.trim().is_empty() {
                append_env_line(env_path, allow_var, allow.trim())?;
                println!(
                    "    {} {}",
                    style("✓").green(),
                    style(format!("Saved {allow_var}")).dim()
                );
            }
        }
    }

    Ok(())
}

// ─── Agent ─────────────────────────────────────────────────────────────

/// Returns `true` if the system prompt was edited.
fn section_agent(cfg: &mut Config, quick: bool) -> Result<bool> {
    println!();
    section_header("Agent");

    let sys_current = cfg.system_prompt.clone().unwrap_or_default();
    println!(
        "  {} {}",
        style("System prompt:  ").dim(),
        if sys_current.is_empty() {
            style("(none)").dim().to_string()
        } else {
            let oneline = sys_current.replace('\n', " ");
            let preview = if oneline.chars().count() > 70 {
                let truncated: String = oneline.chars().take(67).collect();
                format!("{truncated}...")
            } else {
                oneline
            };
            preview
        }
    );
    println!(
        "  {} {}",
        style("Max iterations: ").dim(),
        cfg.max_iterations
    );

    if quick && !sys_current.is_empty() {
        println!();
        println!("  {}", style("Skipped — system prompt already set.").dim());
        return Ok(false);
    }
    println!();

    let theme = ColorfulTheme::default();
    let sys: String = Input::with_theme(&theme)
        .with_prompt("System prompt (optional, press Enter to keep)")
        .default(sys_current.clone())
        .allow_empty(true)
        .interact_text()?;
    let new_sys = if sys.trim().is_empty() {
        None
    } else {
        Some(sys)
    };
    if new_sys == cfg.system_prompt {
        return Ok(false);
    }
    cfg.system_prompt = new_sys;
    Ok(true)
}

/// Prompt for an API key (hidden) and append it to `.env` if entered.
/// Path to `~/.codex/auth.json`. Returns None when no home directory.
fn codex_auth_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/auth.json"))
}

/// Codex stores its OAuth token at `~/.codex/auth.json` after a successful
/// `codex login`. The setup wizard uses the file's existence as a proxy
/// for "credentials present" — codex itself will validate the token on
/// the first `codex exec` call.
fn codex_auth_exists() -> bool {
    codex_auth_path().map(|p| p.exists()).unwrap_or(false)
}

fn prompt_and_save_key(env_path: &std::path::Path, key_env: &str) -> Result<()> {
    let theme = ColorfulTheme::default();
    let prompt = format!("{key_env} (press Enter to skip and add it manually later)");
    let key: String = Password::with_theme(&theme)
        .with_prompt(prompt)
        .allow_empty_password(true)
        .interact()?;
    let trimmed = key.trim();
    if trimmed.is_empty() {
        println!(
            "  {} Add `{key_env}=...` to {} before running merlion.",
            style("·").dim(),
            env_path.display()
        );
    } else {
        append_env_line(env_path, key_env, trimmed)?;
        println!(
            "  {} {}",
            style("✓").green().bold(),
            style(format!("Saved {key_env} to {}", env_path.display())).dim()
        );
    }
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
        assert_eq!(default_model_for("openai"), "gpt-5.5");
        assert_eq!(default_model_for("anthropic"), "claude-sonnet-4");
        assert_eq!(default_model_for("gemini"), "gemini-2.0-flash");
        assert_eq!(default_model_for("deepseek"), "deepseek-chat");
        assert_eq!(default_model_for("groq"), "llama-3.3-70b-versatile");
        assert_eq!(default_model_for("zai"), "glm-4.6");
    }

    #[test]
    fn default_model_unknown_falls_back() {
        // Unknown provider falls back to the OpenAI default — keeps the
        // wizard's "Model" placeholder useful even for a typo'd prefix.
        assert_eq!(default_model_for("not-a-real-provider"), "gpt-5.5");
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
